extern crate nix;

use std::{mem, ptr, slice};
use std::os::unix::io::RawFd;
use nix::{errno, unistd};
use nix::fcntl::{self, FdFlag, FcntlArg};
use nix::sys::uio::IoVec;
use nix::sys::socket::{
    recvmsg, sendmsg, CmsgSpace, ControlMessage, MsgFlags,
    socketpair, AddressFamily, SockFlag, SockType,
};

pub struct Socket {
    fd: RawFd,
}

impl Socket {
    /// Creates a socket pair (AF_UNIX/SOCK_SEQPACKET).
    ///
    /// Both sockets are close-on-exec by default.
    pub fn new_socketpair() -> nix::Result<(Socket, Socket)> {
        socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).map(|(a, b)| {
            (Self::from_raw(a), Self::from_raw(b))
        })
    }

    /// Wraps an existing file descriptor in a Socket.
    pub fn from_raw(fd: RawFd) -> Socket {
        Socket {
            fd,
        }
    }

    /// Disables close-on-exec on the socket (to preserve it across process forks).
    pub fn no_cloexec(&mut self) -> nix::Result<()> {
        fcntl::fcntl(self.fd, FcntlArg::F_SETFD(FdFlag::empty())).map(|_| ())
    }

    /// Returns the underlying file descriptor.
    ///
    /// You can use it to poll with poll/select/kqueue/epoll/whatever, mio, etc.
    pub fn raw_fd(&self) -> RawFd {
        self.fd
    }

    /// Reads bytes from the socket into the given scatter/gather array.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    pub fn recv_into_iovec<F: Default + AsMut<[RawFd]>>(&mut self, iov: &[IoVec<&mut [u8]>]) -> nix::Result<(usize, Option<F>)> {
        let mut rfds = None;
        let mut cmsgspace: CmsgSpace<F> = CmsgSpace::new();
        let msg = recvmsg(self.fd, iov, Some(&mut cmsgspace), MsgFlags::MSG_CMSG_CLOEXEC)?;
        for cmsg in msg.cmsgs() {
            if let ControlMessage::ScmRights(fds) = cmsg {
                if fds.len() >= 1 {
                    let mut fd_arr: F = Default::default();
                    <F as AsMut<[RawFd]>>::as_mut(&mut fd_arr).clone_from_slice(fds);
                    rfds = Some(fd_arr);
                }
            }
        }
        Ok((msg.bytes, rfds))
    }

    /// Reads bytes from the socket into the given buffer.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    pub fn recv_into_slice<F: Default + AsMut<[RawFd]>>(&mut self, buf: &mut [u8]) -> nix::Result<(usize, Option<F>)> {
        let iov = [IoVec::from_mut_slice(&mut buf[..])];
        self.recv_into_iovec(&iov)
    }

    /// Reads bytes from the socket and interprets them as a given data type.
    /// If the size does not match, returns ENOMSG.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    pub fn recv_struct<T, F: Default + AsMut<[RawFd]>>(&mut self) -> nix::Result<(T, Option<F>)> {
        let mut buf = vec![0u8; mem::size_of::<T>()];
        let (bytes, rfds) = self.recv_into_slice(&mut buf[..])?;
        if bytes != mem::size_of::<T>() {
            return Err(nix::Error::Sys(errno::Errno::ENOMSG));
        }
        Ok((unsafe { ptr::read(buf.as_slice().as_ptr() as *const _) }, rfds))
    }

    /// Sends bytes from scatter-gather vectors over the socket.
    ///
    /// Optionally passes file descriptors with the message.
    pub fn send_iovec(&mut self, iov: &[IoVec<&[u8]>], fds: Option<&[RawFd]>) -> nix::Result<usize> {
        if let Some(rfds) = fds {
            sendmsg(self.fd, iov, &[ControlMessage::ScmRights(rfds)], MsgFlags::empty(), None)
        } else {
            sendmsg(self.fd, iov, &[], MsgFlags::empty(), None)
        }
    }

    /// Sends bytes from a slice over the socket.
    ///
    /// Optionally passes file descriptors with the message.
    pub fn send_slice(&mut self, data: &[u8], fds: Option<&[RawFd]>) -> nix::Result<usize> {
        let iov = [IoVec::from_slice(data)];
        self.send_iovec(&iov[..], fds)
    }

    /// Sends bytes from a slice over the socket, prefixing with the length
    /// (as a 64-bit unsigned integer).
    ///
    /// Optionally passes file descriptors with the message.
    pub fn send_slice_with_len(&mut self, data: &[u8], fds: Option<&[RawFd]>) -> nix::Result<usize> {
        let len = data.len() as u64;
        let iov = [IoVec::from_slice(unsafe { slice::from_raw_parts((&len as *const u64) as *const u8, mem::size_of::<u64>()) }), IoVec::from_slice(data)];
        self.send_iovec(&iov[..], fds)
    }

    /// Sends a value of any type as its raw bytes over the socket.
    /// (Do not use with types that contain pointers, references, boxes, etc.!
    ///  Use serialization in that case!)
    ///
    /// Optionally passes file descriptors with the message.
    pub fn send_struct<T>(&mut self, data: &T, fds: Option<&[RawFd]>) -> nix::Result<usize> {
        self.send_slice(unsafe { slice::from_raw_parts((data as *const T) as *const u8, mem::size_of::<T>()) }, fds)
    }

}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = unistd::close(self.fd);
    }
}
