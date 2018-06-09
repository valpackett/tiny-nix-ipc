extern crate nix;
#[cfg(any(feature = "cbor"))]
extern crate serde;
#[cfg(feature = "cbor")]
extern crate serde_cbor;

use std::{mem, ptr, slice};
use std::os::unix::io::{RawFd, FromRawFd, IntoRawFd, AsRawFd};
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

impl FromRawFd for Socket {
    unsafe fn from_raw_fd(fd: RawFd) -> Socket {
        Socket {
            fd,
        }
    }
}

impl IntoRawFd for Socket {
    fn into_raw_fd(self) -> RawFd {
        self.fd
    }
}

impl AsRawFd for Socket {
    fn as_raw_fd(&self) -> RawFd {
        self.fd
    }
}

impl Socket {
    /// Creates a socket pair (AF_UNIX/SOCK_SEQPACKET).
    ///
    /// Both sockets are close-on-exec by default.
    pub fn new_socketpair() -> nix::Result<(Socket, Socket)> {
        socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).map(|(a, b)| {
            unsafe { (Self::from_raw_fd(a), Self::from_raw_fd(b)) }
        })
    }

    /// Disables close-on-exec on the socket (to preserve it across process forks).
    pub fn no_cloexec(&mut self) -> nix::Result<()> {
        fcntl::fcntl(self.fd, FcntlArg::F_SETFD(FdFlag::empty())).map(|_| ())
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

    /// Reads bytes from the socket into a new buffer.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    pub fn recv_into_buf<F: Default + AsMut<[RawFd]>>(&mut self, buf_size: usize) -> nix::Result<(usize, Vec<u8>, Option<F>)> {
        let mut buf = vec![0u8; buf_size];
        let (bytes, rfds) = {
            let iov = [IoVec::from_mut_slice(&mut buf[..])];
            self.recv_into_iovec(&iov)?
        };
        Ok((bytes, buf, rfds))
    }

    /// Reads bytes from the socket into a new buffer, also reading the first 64 bits as length.
    /// The resulting buffer is truncated to that length.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    pub fn recv_into_buf_with_len<F: Default + AsMut<[RawFd]>>(&mut self, buf_size: usize) -> nix::Result<(usize, Vec<u8>, u64, Option<F>)> {
        let mut len: u64 = 0;
        let mut buf = vec![0u8; buf_size];
        let (bytes, rfds) = {
            let iov = [
                IoVec::from_mut_slice(unsafe { slice::from_raw_parts_mut((&mut len as *mut u64) as *mut u8, mem::size_of::<u64>()) }),
                IoVec::from_mut_slice(&mut buf[..]),
            ];
            self.recv_into_iovec(&iov)?
        };
        if bytes != len as usize + mem::size_of::<u64>() {
            return Err(nix::Error::Sys(errno::Errno::ENOMSG));
        }
        buf.truncate(len as usize);
        Ok((bytes, buf, len, rfds))
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
        let (bytes, buf, rfds) = self.recv_into_buf(mem::size_of::<T>())?;
        if bytes != mem::size_of::<T>() {
            return Err(nix::Error::Sys(errno::Errno::ENOMSG));
        }
        Ok((unsafe { ptr::read(buf.as_slice().as_ptr() as *const _) }, rfds))
    }

    /// Reads bytes from the socket and deserializes them as a given data type using CBOR.
    /// If the size does not match, returns ENOMSG.
    ///
    /// You have to provide a size for the receive buffer.
    /// It should be large enough for the data you want to receive plus 64 bits for the length.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    #[cfg(feature = "cbor")]
    pub fn recv_cbor<T: serde::de::DeserializeOwned, F: Default + AsMut<[RawFd]>>(&mut self, buf_size: usize) -> nix::Result<(T, Option<F>)> {
        let (_, buf, _, rfds) = self.recv_into_buf_with_len(buf_size)?;
        Ok((serde_cbor::from_slice(&buf[..]).unwrap(), rfds))
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

    /// Serializes a value with CBOR and sends it over the socket.
    ///
    /// Optionally passes file descriptors with the message.
    #[cfg(feature = "cbor")]
    pub fn send_cbor<T: serde::ser::Serialize>(&mut self, data: &T, fds: Option<&[RawFd]>) -> nix::Result<usize> {
        let bytes = serde_cbor::to_vec(data).unwrap(); // XXX
        self.send_slice_with_len(&bytes[..], fds)
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        let _ = unistd::close(self.fd);
    }
}

#[cfg(test)]
mod tests {
    extern crate shmemfdrs;
    use super::Socket;
    use std::os::unix::io::RawFd;

    #[test]
    fn test_slice_success() {
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let sent = tx.send_slice(&data[..], None).unwrap();
        assert_eq!(sent, 4);
        let mut rdata = [0; 4];
        let (recvd, rfds) = rx.recv_into_slice::<[RawFd; 0]>(&mut rdata[..]).unwrap();
        assert_eq!(recvd, 4);
        assert_eq!(rfds, None);
        assert_eq!(&rdata[..], &data[..]);
    }

    #[test]
    fn test_slice_buf_too_short() {
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let sent = tx.send_slice(&data[..], None).unwrap();
        assert_eq!(sent, 4);
        let mut rdata = [0; 3];
        let (recvd, rfds) = rx.recv_into_slice::<[RawFd; 0]>(&mut rdata[..]).unwrap();
        assert_eq!(recvd, 3);
        assert_eq!(rfds, None);
        assert_eq!(&rdata[..], &data[0..3]);
    }

    #[test]
    fn test_slice_with_len_success() {
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let sent = tx.send_slice_with_len(&data[..], None).unwrap();
        assert_eq!(sent, 12); // 4 + 8 (bytes in a 64-bit number)
        let mut rdata = [0; 12];
        let (recvd, rfds) = rx.recv_into_slice::<[RawFd; 0]>(&mut rdata[..]).unwrap();
        assert_eq!(recvd, 12);
        assert_eq!(rfds, None);
        assert_eq!(rdata[0], 4);
        assert_eq!(&rdata[8..], &data[..]);
    }

    #[derive(Debug, PartialEq)]
    struct TestStruct {
        one: i8,
        two: u32,
    }

    #[test]
    fn test_struct_success() {
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = TestStruct { one: -64, two: 0xDEADBEEF, };
        let _ = tx.send_struct(&data, None).unwrap();
        let (rdata, rfds) = rx.recv_struct::<TestStruct, [RawFd; 0]>().unwrap();
        assert_eq!(rfds, None);
        assert_eq!(rdata, data);
    }

    #[test]
    fn test_struct_wrong_len() {
        use nix::{errno, Error};
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let sent = tx.send_slice(&data[..], None).unwrap();
        assert_eq!(sent, 4);
        let ret = rx.recv_struct::<TestStruct, [RawFd; 0]>();
        assert_eq!(ret, Err(Error::Sys(errno::Errno::ENOMSG)));
    }

    #[test]
    fn test_fd_passing() {
        use std::fs::File;
        use std::io::{Read, Write, Seek, SeekFrom};
        use std::os::unix::io::FromRawFd;
        use std::ffi::CString;
        use std::mem::ManuallyDrop;
        let fd = shmemfdrs::create_shmem(CString::new("/test").unwrap(), 6);
        let mut orig_file = {
            let mut file = unsafe { File::from_raw_fd(fd) };
            file.write_all(b"hello\n").unwrap();
            ManuallyDrop::new(file) // do not destroy the actual file before it's read
        };
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let sent = tx.send_slice(&data[..], Some(&[fd])).unwrap();
        assert_eq!(sent, 4);
        let mut rdata = [0; 4];
        let (recvd, rfds) = rx.recv_into_slice::<[RawFd; 1]>(&mut rdata[..]).unwrap();
        assert_eq!(recvd, 4);
        assert_eq!(&rdata[..], &data[..]);
        let new_fd = rfds.unwrap()[0];
        {
            let mut file = unsafe { File::from_raw_fd(new_fd) };
            let mut content = String::new();
            file.seek(SeekFrom::Start(0)).unwrap();
            file.read_to_string(&mut content).unwrap();
            assert_eq!(content, "hello\n");
        }
        unsafe { ManuallyDrop::drop(&mut orig_file); }
    }

    #[test]
    #[cfg(feature = "cbor")]
    fn test_cbor() {
        use serde_cbor::value::Value;
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = Value::U64(123456);
        let _ = tx.send_cbor(&data, None).unwrap();
        let (rdata, rfds) = rx.recv_cbor::<Value, [RawFd; 0]>(24).unwrap();
        assert_eq!(rfds, None);
        assert_eq!(rdata, data);
    }
}
