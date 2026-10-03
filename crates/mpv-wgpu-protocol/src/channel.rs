//! The socket: one message per `SOCK_SEQPACKET` packet, a memfd by `SCM_RIGHTS`.

use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, Shutdown, SocketAddrUnix, SocketFlags, SocketType,
    accept_with, bind, connect, listen, recvmsg, sendmsg, socket_with, sockopt,
};

use crate::message::{DecodeError, MAX_MESSAGE, Message};

/// Socket buffer size. The kernel doubles it. One packet must fit.
const BUFFER_BYTES: usize = 2 * MAX_MESSAGE;

fn invalid(error: DecodeError) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

/// `CLOCK_MONOTONIC` in nanoseconds. Both processes share the clock.
pub fn monotonic_ns() -> u64 {
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    (now.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(now.tv_nsec as u64)
}

/// A connected `SOCK_SEQPACKET` socket. Sending and receiving take `&self`, so
/// one thread can read while others write. Each send is one atomic packet.
#[derive(Debug)]
pub struct Channel {
    fd: OwnedFd,
}

impl Channel {
    fn new(fd: OwnedFd) -> io::Result<Self> {
        sockopt::set_socket_send_buffer_size(&fd, BUFFER_BYTES)?;
        sockopt::set_socket_recv_buffer_size(&fd, BUFFER_BYTES)?;
        Ok(Self { fd })
    }

    /// Connect to the abstract socket `name`, as the plugin does.
    pub fn connect_abstract(name: &str) -> io::Result<Self> {
        let fd = socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        connect(&fd, &SocketAddrUnix::new_abstract_name(name.as_bytes())?)?;
        Self::new(fd)
    }

    /// Send `message`, with `fd` attached when there is one.
    pub fn send(&self, message: &Message, fd: Option<BorrowedFd<'_>>) -> io::Result<()> {
        let mut packet = Vec::with_capacity(64);
        message
            .encode(&mut packet)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = SendAncillaryBuffer::new(&mut space);
        let fds = fd.map(|fd| [fd]);
        if let Some(fds) = &fds {
            ancillary.push(SendAncillaryMessage::ScmRights(fds));
        }
        let sent = sendmsg(
            &self.fd,
            &[IoSlice::new(&packet)],
            &mut ancillary,
            SendFlags::NOSIGNAL,
        )?;
        if sent != packet.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "packet was sent in part",
            ));
        }
        Ok(())
    }

    /// Block for the next message. `Ok(None)` is an orderly close.
    ///
    /// `buffer` is scratch space the caller keeps so receiving does not allocate.
    pub fn recv(&self, buffer: &mut Vec<u8>) -> io::Result<Option<(Message, Option<OwnedFd>)>> {
        buffer.resize(MAX_MESSAGE + 1, 0);
        let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let received = recvmsg(
            &self.fd,
            &mut [IoSliceMut::new(buffer)],
            &mut ancillary,
            RecvFlags::CMSG_CLOEXEC,
        )?;
        let mut fd = None;
        for message in ancillary.drain() {
            if let RecvAncillaryMessage::ScmRights(mut fds) = message {
                fd = fd.or(fds.next());
            }
        }
        if received.bytes == 0 {
            return Ok(None);
        }
        if received.flags.contains(rustix::net::ReturnFlags::TRUNC) || received.bytes > MAX_MESSAGE
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "packet is larger than the maximum message",
            ));
        }
        let message = Message::decode(&buffer[..received.bytes]).map_err(invalid)?;
        Ok(Some((message, fd)))
    }

    /// Shut both directions, which wakes a thread blocked in [`Channel::recv`].
    pub fn shutdown(&self) {
        let _ = rustix::net::shutdown(&self.fd, Shutdown::Both);
    }
}

/// A listening abstract socket the plugin connects to.
#[derive(Debug)]
pub struct Listener {
    fd: OwnedFd,
    name: String,
}

impl Listener {
    /// Listen on a fresh abstract name: 128 random bits in hex.
    pub fn bind_random() -> io::Result<Self> {
        let mut random = [0u8; 16];
        let mut filled = 0;
        while filled < random.len() {
            let got = rustix::rand::getrandom(
                &mut random[filled..],
                rustix::rand::GetRandomFlags::empty(),
            )?;
            filled += got;
        }
        let name = format!(
            "mpv-wgpu-{}",
            random
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        );
        let fd = socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        bind(&fd, &SocketAddrUnix::new_abstract_name(name.as_bytes())?)?;
        listen(&fd, 1)?;
        Ok(Self { fd, name })
    }

    /// The abstract name, without the leading NUL, to hand to the plugin.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Wait up to `timeout` for a peer of the same user. `Ok(None)` is a timeout.
    pub fn accept_timeout(&self, timeout: Duration) -> io::Result<Option<Channel>> {
        let mut fds = [PollFd::new(&self.fd, PollFlags::IN)];
        let limit = rustix::event::Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        let ready = poll(&mut fds, Some(&limit))?;
        if ready == 0 {
            return Ok(None);
        }
        let fd = accept_with(&self.fd, SocketFlags::CLOEXEC)?;
        let credentials = sockopt::socket_peercred(&fd)?;
        if credentials.uid != rustix::process::getuid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the peer is another user",
            ));
        }
        Channel::new(fd).map(Some)
    }
}

impl AsFd for Channel {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::thread;

    use super::*;
    use crate::message::Value;

    fn pair() -> (Channel, Channel) {
        let listener = Listener::bind_random().unwrap();
        let name = listener.name().to_string();
        let client = thread::spawn(move || Channel::connect_abstract(&name).unwrap());
        let server = loop {
            if let Some(channel) = listener.accept_timeout(Duration::from_millis(100)).unwrap() {
                break channel;
            }
        };
        (server, client.join().unwrap())
    }

    #[test]
    fn messages_cross_in_both_directions() {
        let (a, b) = pair();
        let mut scratch = Vec::new();
        a.send(&Message::Dropped { count: 5 }, None).unwrap();
        b.send(
            &Message::Property {
                name: "volume".into(),
                value: Value::Double(50.0),
            },
            None,
        )
        .unwrap();
        let (got, fd) = b.recv(&mut scratch).unwrap().unwrap();
        assert_eq!(got, Message::Dropped { count: 5 });
        assert!(fd.is_none());
        let (got, _) = a.recv(&mut scratch).unwrap().unwrap();
        assert!(matches!(got, Message::Property { .. }));
    }

    #[test]
    fn packets_keep_their_boundaries() {
        let (a, b) = pair();
        let mut scratch = Vec::new();
        for count in 0..50 {
            a.send(&Message::Dropped { count }, None).unwrap();
        }
        for count in 0..50 {
            let (got, _) = b.recv(&mut scratch).unwrap().unwrap();
            assert_eq!(got, Message::Dropped { count });
        }
    }

    #[test]
    fn a_descriptor_travels_with_its_message() {
        let (a, b) = pair();
        let mut scratch = Vec::new();
        let ring = crate::Ring::create(crate::RingLayout::new(1, 4, 2, 256, 3).unwrap()).unwrap();
        a.send(&Message::Bye, Some(ring.fd())).unwrap();
        let (got, fd) = b.recv(&mut scratch).unwrap().unwrap();
        assert_eq!(got, Message::Bye);
        assert!(fd.is_some());
    }

    #[test]
    fn a_close_is_none_and_an_oversized_message_is_refused() {
        let (a, b) = pair();
        let mut scratch = Vec::new();
        let big = Message::Log {
            level: 0,
            prefix: String::new(),
            text: "x".repeat(MAX_MESSAGE),
        };
        assert!(a.send(&big, None).is_err());
        drop(a);
        assert!(b.recv(&mut scratch).unwrap().is_none());
    }

    #[test]
    fn garbage_is_invalid_data() {
        let (a, b) = pair();
        let mut scratch = Vec::new();
        let sent = sendmsg(
            &a.fd,
            &[IoSlice::new(&[0xee, 1, 2])],
            &mut SendAncillaryBuffer::default(),
            SendFlags::empty(),
        )
        .unwrap();
        assert_eq!(sent, 3);
        let error = b.recv(&mut scratch).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn accept_times_out_without_a_peer() {
        let listener = Listener::bind_random().unwrap();
        assert!(
            listener
                .accept_timeout(Duration::from_millis(10))
                .unwrap()
                .is_none()
        );
    }
}
