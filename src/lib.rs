extern crate nix;
#[macro_use]
extern crate error_chain;
#[cfg(any(feature = "ser_cbor", feature = "ser_json", feature = "ser_bincode"))]
extern crate serde;
#[cfg(feature = "ser_cbor")]
extern crate serde_cbor;
#[cfg(feature = "ser_json")]
extern crate serde_json;
#[cfg(feature = "ser_bincode")]
extern crate bincode;
#[cfg(feature = "zero_copy")]
#[macro_use]
extern crate zerocopy;

use std::{mem, ptr, slice};
use std::os::unix::io::{RawFd, FromRawFd, IntoRawFd, AsRawFd};
use nix::{unistd, cmsg_space};
use nix::fcntl::{self, FdFlag, FcntlArg};
use nix::sys::uio::IoVec;
use nix::sys::socket::{
    recvmsg, sendmsg, ControlMessageOwned, ControlMessage, MsgFlags,
    socketpair, AddressFamily, SockFlag, SockType,
};

pub mod errors {
    error_chain!{
        foreign_links {
            Nix(::nix::Error);
            Cbor(::serde_cbor::error::Error) #[cfg(feature = "ser_cbor")];
            Json(::serde_json::Error) #[cfg(feature = "ser_json")];
            Bincode(::bincode::Error) #[cfg(feature = "ser_bincode")];
        }

        errors {
            WrongRecvLength {
                description("length of received message doesn't match the struct size or received length")
            }
        }
    }
}

use errors::*;

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
        let fd = self.fd;
        std::mem::forget(self);
        fd
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
    pub fn new_socketpair() -> Result<(Socket, Socket)> {
        socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).map(|(a, b)| {
            unsafe { (Self::from_raw_fd(a), Self::from_raw_fd(b)) }
        }).map_err(|e| e.into())
    }

    /// Disables close-on-exec on the socket (to preserve it across process forks).
    pub fn no_cloexec(&mut self) -> Result<()> {
        fcntl::fcntl(self.fd, FcntlArg::F_SETFD(FdFlag::empty())).map(|_| ()).map_err(|e| e.into())
    }

    /// Reads bytes from the socket into the given scatter/gather array.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    pub fn recv_into_iovec<F: Default + AsMut<[RawFd]>>(&mut self, iov: &[IoVec<&mut [u8]>]) -> Result<(usize, Option<F>)> {
        let mut rfds = None;
        let mut cmsgspace = cmsg_space!(F);
        let msg = recvmsg(self.fd, iov, Some(&mut cmsgspace), MsgFlags::MSG_CMSG_CLOEXEC)?;
        for cmsg in msg.cmsgs() {
            if let ControlMessageOwned::ScmRights(fds) = cmsg {
                if fds.len() >= 1 {
                    let mut fd_arr: F = Default::default();
                    <F as AsMut<[RawFd]>>::as_mut(&mut fd_arr).clone_from_slice(&fds);
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
    pub fn recv_into_slice<F: Default + AsMut<[RawFd]>>(&mut self, buf: &mut [u8]) -> Result<(usize, Option<F>)> {
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
    pub fn recv_into_buf<F: Default + AsMut<[RawFd]>>(&mut self, buf_size: usize) -> Result<(usize, Vec<u8>, Option<F>)> {
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
    pub fn recv_into_buf_with_len<F: Default + AsMut<[RawFd]>>(&mut self, buf_size: usize) -> Result<(usize, Vec<u8>, u64, Option<F>)> {
        let mut len: u64 = 0;
        let mut buf = vec![0u8; buf_size];
        let (bytes, rfds) = {
            let iov = [
                IoVec::from_mut_slice(unsafe { slice::from_raw_parts_mut((&mut len as *mut u64) as *mut u8, mem::size_of::<u64>()) }),
                IoVec::from_mut_slice(&mut buf[..]),
            ];
            self.recv_into_iovec(&iov)?
        };
        buf.truncate(len as usize);
        Ok((bytes, buf, len, rfds))
    }


    /// See `recv_struct` for docs
    ///
    /// # Safety
    /// - For some types (e.g.), not every bit pattern is allowed. If bytes, read from socket
    /// aren't correct, that's UB.
    /// - Some types mustn't change their memory location (see `std::pin::Pin`). Sending object of
    /// such a type is UB.
    pub unsafe fn recv_struct_raw<T, F: Default + AsMut<[RawFd]>>(&mut self) -> Result<(T, Option<F>)> {
        let (bytes, buf, rfds) = self.recv_into_buf(mem::size_of::<T>())?;
        if bytes != mem::size_of::<T>() {
            bail!(ErrorKind::WrongRecvLength);
        }
        Ok((ptr::read(buf.as_slice().as_ptr() as *const _), rfds))
    }
    
    /// Reads bytes from the socket and interprets them as a given data type.
    /// If the size does not match, returns `WrongRecvLength`..
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    #[cfg(feature = "zero_copy")]
    pub fn recv_struct<T: zerocopy::FromBytes, F: Default + AsMut<[RawFd]>>(&mut self) -> Result<(T, Option<F>)> {
        unsafe {
            self.recv_struct_raw()
        }
    }

    /// Reads bytes from the socket and deserializes them as a given data type using CBOR.
    /// If the size does not match, returns `WrongRecvLength`.
    ///
    /// You have to provide a size for the receive buffer.
    /// It should be large enough for the data you want to receive plus 64 bits for the length.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    #[cfg(feature = "ser_cbor")]
    pub fn recv_cbor<T: serde::de::DeserializeOwned, F: Default + AsMut<[RawFd]>>(&mut self, buf_size: usize) -> Result<(T, Option<F>)> {
        let (bytes, buf, len, rfds) = self.recv_into_buf_with_len(buf_size)?;
        if bytes != len as usize + mem::size_of::<u64>() {
            bail!(ErrorKind::WrongRecvLength);
        }
        Ok((serde_cbor::from_slice(&buf[..])?, rfds))
    }

    /// Reads bytes from the socket and deserializes them as a given data type using JSON.
    /// If the size does not match, returns `WrongRecvLength`.
    ///
    /// You have to provide a size for the receive buffer.
    /// It should be large enough for the data you want to receive plus 64 bits for the length.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    #[cfg(feature = "ser_json")]
    pub fn recv_json<T: serde::de::DeserializeOwned, F: Default + AsMut<[RawFd]>>(&mut self, buf_size: usize) -> Result<(T, Option<F>)> {
        let (bytes, buf, len, rfds) = self.recv_into_buf_with_len(buf_size)?;
        if bytes != len as usize + mem::size_of::<u64>() {
            bail!(ErrorKind::WrongRecvLength);
        }
        Ok((serde_json::from_slice(&buf[..])?, rfds))
    }

    /// Reads bytes from the socket and deserializes them as a given data type using Bincode.
    /// If the size does not match, returns `WrongRecvLength`.
    ///
    /// You have to provide a size for the receive buffer.
    /// It should be large enough for the data you want to receive plus 64 bits for the length.
    ///
    /// If file descriptors were passed, returns them too.
    /// To receive file descriptors, you need to instantiate the type parameter `F`
    /// as `[RawFd; n]`, where `n` is the number of descriptors you want to receive.
    ///
    /// Received file descriptors are set close-on-exec.
    #[cfg(feature = "ser_bincode")]
    pub fn recv_bincode<T: serde::de::DeserializeOwned, F: Default + AsMut<[RawFd]>>(&mut self, buf_size: usize) -> Result<(T, Option<F>)> {
        let (bytes, buf, len, rfds) = self.recv_into_buf_with_len(buf_size)?;
        if bytes != len as usize + mem::size_of::<u64>() {
            bail!(ErrorKind::WrongRecvLength);
        }
        Ok((bincode::deserialize(&buf[..])?, rfds))
    }

    /// Sends bytes from scatter-gather vectors over the socket.
    ///
    /// Optionally passes file descriptors with the message.
    pub fn send_iovec(&mut self, iov: &[IoVec<&[u8]>], fds: Option<&[RawFd]>) -> Result<usize> {
        if let Some(rfds) = fds {
            sendmsg(self.fd, iov, &[ControlMessage::ScmRights(rfds)], MsgFlags::empty(), None).map_err(|e| e.into())
        } else {
            sendmsg(self.fd, iov, &[], MsgFlags::empty(), None).map_err(|e| e.into())
        }
    }

    /// Sends bytes from a slice over the socket.
    ///
    /// Optionally passes file descriptors with the message.
    pub fn send_slice(&mut self, data: &[u8], fds: Option<&[RawFd]>) -> Result<usize> {
        let iov = [IoVec::from_slice(data)];
        self.send_iovec(&iov[..], fds)
    }

    /// Sends bytes from a slice over the socket, prefixing with the length
    /// (as a 64-bit unsigned integer).
    ///
    /// Optionally passes file descriptors with the message.
    pub fn send_slice_with_len(&mut self, data: &[u8], fds: Option<&[RawFd]>) -> Result<usize> {
        let len = data.len() as u64;
        let iov = [IoVec::from_slice(unsafe { slice::from_raw_parts((&len as *const u64) as *const u8, mem::size_of::<u64>()) }), IoVec::from_slice(data)];
        self.send_iovec(&iov[..], fds)
    }

    /// See `send_struct` for docs.
    ///
    /// # Safety
    /// - T must not have padding bytes.
    /// - Also, if T violates `recv_struct_raw` safety preconditions, receiving it will trigger
    /// undefined behavior.
    pub unsafe fn send_struct_raw<T>(&mut self, data: &T, fds: Option<&[RawFd]>) -> Result<usize> {
        self.send_slice(slice::from_raw_parts((data as *const T) as *const u8, mem::size_of::<T>()), fds)
    }

    /// Sends a value of any type as its raw bytes over the socket.
    /// (Do not use with types that contain pointers, references, boxes, etc.!
    ///  Use serialization in that case!)
    ///
    /// Optionally passes file descriptors with the message.
    #[cfg(feature = "zero_copy")]
    pub fn send_struct<T: zerocopy::AsBytes>(&mut self, data: &T, fds: Option<&[RawFd]>) -> Result<usize> {
        unsafe {
            self.send_struct_raw(data, fds)
        }
    }



    /// Serializes a value with CBOR and sends it over the socket.
    ///
    /// Optionally passes file descriptors with the message.
    #[cfg(feature = "ser_cbor")]
    pub fn send_cbor<T: serde::ser::Serialize>(&mut self, data: &T, fds: Option<&[RawFd]>) -> Result<usize> {
        let bytes = serde_cbor::to_vec(data)?;
        self.send_slice_with_len(&bytes[..], fds)
    }

    /// Serializes a value with JSON and sends it over the socket.
    ///
    /// Optionally passes file descriptors with the message.
    #[cfg(feature = "ser_json")]
    pub fn send_json<T: serde::ser::Serialize>(&mut self, data: &T, fds: Option<&[RawFd]>) -> Result<usize> {
        let bytes = serde_json::to_vec(data)?;
        self.send_slice_with_len(&bytes[..], fds)
    }

    /// Serializes a value with Bincode and sends it over the socket.
    ///
    /// Optionally passes file descriptors with the message.
    #[cfg(feature = "ser_bincode")]
    pub fn send_bincode<T: serde::ser::Serialize>(&mut self, data: &T, fds: Option<&[RawFd]>) -> Result<usize> {
        let bytes = bincode::serialize(data)?;
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
    #[cfg(feature = "zero_copy")]
    use zerocopy::AsBytes;

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

    #[cfg(feature = "zero_copy")]
    #[derive(Debug, PartialEq, FromBytes, AsBytes)]
    #[repr(C)]
    struct TestStruct {
        one: i8,
        // Note an explicit padding bytes here
        // Without it, `send_struct` would read real compiler-provided padding, which is UB
        pad: [u8; 3],
        two: u32,
    }

    #[test]
    #[cfg(feature = "zero_copy")]
    fn test_struct_success() {
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = TestStruct { one: -64, two: 0xDEADBEEF, pad: [0, 0, 0]};
        let _ = tx.send_struct(&data, None).unwrap();
        let (rdata, rfds) = rx.recv_struct::<TestStruct, [RawFd; 0]>().unwrap();
        assert_eq!(rfds, None);
        assert_eq!(rdata, data);
    }

    #[test]
    #[cfg(feature = "zero_copy")]
    fn test_struct_wrong_len() {
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = [0xDE, 0xAD, 0xBE, 0xEF];
        let sent = tx.send_slice(&data[..], None).unwrap();
        assert_eq!(sent, 4);
        let ret = rx.recv_struct::<TestStruct, [RawFd; 0]>();
        assert!(ret.is_err());
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
    #[cfg(feature = "ser_cbor")]
    fn test_cbor() {
        use serde_cbor::value::Value;
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = Value::U64(123456);
        let _ = tx.send_cbor(&data, None).unwrap();
        let (rdata, rfds) = rx.recv_cbor::<Value, [RawFd; 0]>(24).unwrap();
        assert_eq!(rfds, None);
        assert_eq!(rdata, data);
    }

    #[test]
    #[cfg(feature = "ser_json")]
    fn test_json() {
        use serde_json::value::Value;
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = Value::String("hi".to_owned());
        let _ = tx.send_json(&data, None).unwrap();
        let (rdata, rfds) = rx.recv_json::<Value, [RawFd; 0]>(24).unwrap();
        assert_eq!(rfds, None);
        assert_eq!(rdata, data);
    }

    #[test]
    #[cfg(feature = "ser_bincode")]
    fn test_bincode() {
        let (mut rx, mut tx) = Socket::new_socketpair().unwrap();
        let data = Some("hello world".to_string());
        let _ = tx.send_bincode(&data, None).unwrap();
        let (rdata, rfds) = rx.recv_bincode::<Option<String>, [RawFd; 0]>(24).unwrap();
        assert_eq!(rfds, None);
        assert_eq!(rdata, data);
    }
}
