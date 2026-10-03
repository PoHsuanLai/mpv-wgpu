//! The frame ring: one memfd of `slots` equal slots, mapped by both processes.

use std::fmt;
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;

use rustix::fs::{MemfdFlags, fstat, ftruncate, memfd_create};
use rustix::mm::{MapFlags, ProtFlags, mmap, munmap};

/// Slots in a ring: one being read by the player, one being written by mpv,
/// and one in between.
pub const SLOTS: u8 = 3;

/// Largest ring, in bytes (2 GiB). An 8K frame is 128 MB, so three fit easily.
const MAX_RING_BYTES: u64 = 2 << 30;

/// Row alignment the player's texture upload wants.
const ROW_ALIGNMENT: u64 = 256;

/// Bytes in one row of `width` `rgb0` pixels, rounded up to 256.
pub fn row_stride(width: u32) -> Option<u32> {
    let tight = u64::from(width).checked_mul(4)?;
    let aligned = tight.div_ceil(ROW_ALIGNMENT).checked_mul(ROW_ALIGNMENT)?;
    if aligned == 0 {
        return None;
    }
    u32::try_from(aligned).ok()
}

/// A layout that does not describe a usable ring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingError(&'static str);

impl fmt::Display for RingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "bad ring: {}", self.0)
    }
}

impl std::error::Error for RingError {}

impl From<RingError> for io::Error {
    fn from(error: RingError) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, error)
    }
}

/// Geometry of a ring. Every slot is `stride * height` bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RingLayout {
    generation: u32,
    width: u32,
    height: u32,
    stride: u32,
    slots: u8,
}

impl RingLayout {
    /// Check a layout: non-zero size, a stride that holds a row, one to
    /// [`SLOTS`] slots, and a total size under the cap.
    pub fn new(
        generation: u32,
        width: u32,
        height: u32,
        stride: u32,
        slots: u8,
    ) -> Result<Self, RingError> {
        if width == 0 || height == 0 {
            return Err(RingError("empty slot"));
        }
        if u64::from(stride) < u64::from(width) * 4 {
            return Err(RingError("stride is shorter than a row"));
        }
        if slots == 0 || slots > SLOTS {
            return Err(RingError("slot count"));
        }
        let layout = Self {
            generation,
            width,
            height,
            stride,
            slots,
        };
        if layout.total() > MAX_RING_BYTES {
            return Err(RingError("ring is too large"));
        }
        Ok(layout)
    }

    /// The ring's generation.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Bytes per row.
    pub fn stride(&self) -> u32 {
        self.stride
    }

    /// Number of slots.
    pub fn slots(&self) -> u8 {
        self.slots
    }

    /// Bytes in one slot.
    pub fn slot_len(&self) -> usize {
        self.stride as usize * self.height as usize
    }

    fn total(&self) -> u64 {
        u64::from(self.stride) * u64::from(self.height) * u64::from(self.slots)
    }
}

/// A mapped ring. The mapping is shared with the other process, which writes
/// slots the protocol hands it and never one it has handed back.
#[derive(Debug)]
pub struct Ring {
    base: NonNull<u8>,
    len: usize,
    layout: RingLayout,
    fd: OwnedFd,
}

// SAFETY: the mapping is plain bytes owned by this value until drop. Access to a
// slot is serialised by the message protocol (a slot has one writer at a time),
// and the pointer is never aliased by Rust-owned data.
unsafe impl Send for Ring {}
// SAFETY: see `Send`. Shared references only read slots or hand out raw pointers.
unsafe impl Sync for Ring {}

impl Ring {
    /// Create a ring: a new memfd of the layout's size, mapped read-write.
    pub fn create(layout: RingLayout) -> io::Result<Self> {
        let fd = memfd_create("mpv-wgpu-ring", MemfdFlags::CLOEXEC)?;
        ftruncate(&fd, layout.total())?;
        Self::map(fd, layout)
    }

    /// Map a ring received over the socket. The memfd must be at least as large
    /// as the layout says.
    pub fn from_fd(fd: OwnedFd, layout: RingLayout) -> io::Result<Self> {
        let size = fstat(&fd)?.st_size;
        if u64::try_from(size).map_err(|_| io::Error::other("negative size"))? < layout.total() {
            return Err(RingError("memfd is smaller than the layout").into());
        }
        Self::map(fd, layout)
    }

    fn map(fd: OwnedFd, layout: RingLayout) -> io::Result<Self> {
        let len = usize::try_from(layout.total())
            .map_err(|_| io::Error::from(RingError("ring is too large")))?;
        // SAFETY: a null hint with a fresh shared mapping of `len` bytes of a
        // memfd we hold open. The kernel picks the address; the result is
        // checked by `mmap` and unmapped exactly once in `Drop`.
        let pointer = unsafe {
            mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )?
        };
        let base = NonNull::new(pointer.cast::<u8>())
            .ok_or_else(|| io::Error::other("mmap returned null"))?;
        Ok(Self {
            base,
            len,
            layout,
            fd,
        })
    }

    /// The ring's geometry.
    pub fn layout(&self) -> RingLayout {
        self.layout
    }

    /// The memfd, to send with [`crate::Message::Resize`].
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    /// Slot `index` as bytes. Only read a slot the peer has handed over with
    /// [`crate::Message::Frame`] and not yet been given back.
    pub fn slot(&self, index: u8) -> Option<&[u8]> {
        let pointer = self.slot_ptr(index)?;
        // SAFETY: `slot_ptr` bounds-checks `index`, so the slot lies inside the
        // mapping, which lives as long as `self`. The peer does not write a
        // slot between sending its frame and receiving it back.
        Some(unsafe { std::slice::from_raw_parts(pointer.as_ptr(), self.layout.slot_len()) })
    }

    /// Slot `index` as a raw pointer for the writer (mpv's software render).
    /// `None` when `index` is out of range. The pointer is valid for
    /// [`RingLayout::slot_len`] bytes while `self` lives.
    pub fn slot_ptr(&self, index: u8) -> Option<NonNull<u8>> {
        if index >= self.layout.slots {
            return None;
        }
        let offset = self.layout.slot_len() * usize::from(index);
        debug_assert!(offset < self.len);
        // SAFETY: `offset` is below `len` by the check above, inside the mapping.
        Some(unsafe { self.base.add(offset) })
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: `base` and `len` are exactly what `mmap` returned and was asked
        // for, and nothing else unmaps them.
        let _ = unsafe { munmap(self.base.as_ptr().cast(), self.len) };
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn stride_rounds_up_to_256() {
        assert_eq!(row_stride(1), Some(256));
        assert_eq!(row_stride(64), Some(256));
        assert_eq!(row_stride(65), Some(512));
        assert_eq!(row_stride(1920), Some(7680));
        assert_eq!(row_stride(3840), Some(15360));
        assert_eq!(row_stride(0), None);
        assert_eq!(row_stride(u32::MAX), None);
    }

    #[test]
    fn layouts_are_validated() {
        let cases: &[(&str, u32, u32, u32, u8, bool)] = &[
            ("good", 64, 48, 256, 3, true),
            ("zero width", 0, 48, 256, 3, false),
            ("zero height", 64, 0, 256, 3, false),
            ("stride too short", 64, 48, 255, 3, false),
            ("no slots", 64, 48, 256, 0, false),
            ("too many slots", 64, 48, 256, 4, false),
            ("absurd size", 100_000, 100_000, 400_000, 3, false),
        ];
        for (name, w, h, stride, slots, ok) in cases {
            assert_eq!(
                RingLayout::new(1, *w, *h, *stride, *slots).is_ok(),
                *ok,
                "{name}"
            );
        }
    }

    #[test]
    fn two_mappings_of_one_memfd_share_bytes() {
        let layout = RingLayout::new(1, 8, 4, 256, 3).unwrap();
        let writer = Ring::create(layout).unwrap();
        let reader = Ring::from_fd(writer.fd().try_clone_to_owned().unwrap(), layout).unwrap();
        let pointer = writer.slot_ptr(2).unwrap();
        // SAFETY: the slot is in range and valid for `slot_len` bytes; nothing else
        // touches it in this test.
        unsafe { pointer.as_ptr().write(0xAB) };
        assert_eq!(reader.slot(2).unwrap()[0], 0xAB);
        assert_eq!(reader.slot(0).unwrap()[0], 0);
        assert_eq!(reader.slot(2).unwrap().len(), 256 * 4);
        assert!(reader.slot(3).is_none());
    }

    #[test]
    fn a_short_memfd_is_refused() {
        let small = RingLayout::new(1, 8, 4, 256, 1).unwrap();
        let big = RingLayout::new(1, 8, 4, 256, 3).unwrap();
        let ring = Ring::create(small).unwrap();
        let error = Ring::from_fd(ring.fd().try_clone_to_owned().unwrap(), big).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
