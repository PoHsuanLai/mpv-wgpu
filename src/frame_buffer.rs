//! One CPU buffer for libmpv's software target. Capacity only grows.

use crate::types::{Error, SlotSize};

/// Byte alignment required by `wgpu::Queue::write_texture`.
pub const ROW_ALIGNMENT: usize = 256;

/// Bytes in one row of an `Rgb0` image of `width`, rounded up to [`ROW_ALIGNMENT`].
pub fn row_stride(width: u32) -> Result<usize, Error> {
    let tight = (width as usize)
        .checked_mul(4)
        .ok_or(Error::InvalidSize)?;
    let aligned = tight.div_ceil(ROW_ALIGNMENT).saturating_mul(ROW_ALIGNMENT);
    if aligned == 0 {
        return Err(Error::InvalidSize);
    }
    Ok(aligned)
}

/// Packed `Rgb0` pixels for the current slot.
#[derive(Debug)]
pub struct FrameBuffer {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
    stride: usize,
}

impl FrameBuffer {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            width: 0,
            height: 0,
            stride: 0,
        }
    }

    /// Grow or shrink the length to `size`. Capacity never shrinks.
    pub fn ensure(&mut self, size: SlotSize) -> Result<(), Error> {
        let stride = row_stride(size.width.get())?;
        let len = size.byte_len(stride)?;
        if self.bytes.len() != len {
            self.bytes.resize(len, 0);
        }
        self.width = size.width.get();
        self.height = size.height.get();
        self.stride = stride;
        Ok(())
    }

    /// Drop the live pixels and keep the allocation.
    pub fn clear_len(&mut self) {
        self.bytes.clear();
        self.width = 0;
        self.height = 0;
        self.stride = 0;
    }

    pub fn pixels(&self) -> &[u8] {
        &self.bytes
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.bytes
    }

    pub fn stride(&self) -> usize {
        self.stride
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn capacity(&self) -> usize {
        self.bytes.capacity()
    }
}

impl Default for FrameBuffer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::num::NonZeroU32;

    fn size(width: u32, height: u32) -> SlotSize {
        SlotSize {
            width: NonZeroU32::new(width).unwrap(),
            height: NonZeroU32::new(height).unwrap(),
        }
    }

    #[test]
    fn stride_is_a_multiple_of_256() {
        for width in [1, 2, 63, 64, 100, 1920, 3840] {
            let stride = row_stride(width).unwrap();
            assert_eq!(stride % 256, 0, "width {width}");
            assert!(stride >= width as usize * 4);
        }
    }

    #[test]
    fn capacity_stays_put_when_the_slot_is_unchanged() {
        let mut buffer = FrameBuffer::new();
        let slot = size(100, 80);
        buffer.ensure(slot).unwrap();
        let capacity = buffer.capacity();
        assert!(capacity >= buffer.stride() * 80);
        buffer.ensure(slot).unwrap();
        assert_eq!(buffer.capacity(), capacity);
        buffer.pixels_mut().fill(7);
        buffer.ensure(slot).unwrap();
        assert_eq!(buffer.capacity(), capacity);
    }

    #[test]
    fn clear_keeps_capacity() {
        let mut buffer = FrameBuffer::new();
        buffer.ensure(size(32, 32)).unwrap();
        let capacity = buffer.capacity();
        buffer.clear_len();
        assert_eq!(buffer.capacity(), capacity);
        assert_eq!(buffer.stride(), 0);
    }
}
