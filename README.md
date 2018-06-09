[![unlicense](https://img.shields.io/badge/un-license-green.svg?style=flat)](http://unlicense.org)
[![crates.io](https://img.shields.io/crates/v/tiny-nix-ipc.svg)](https://crates.io/crates/tiny-nix-ipc)

# tiny-nix-ipc

A small and convenient Rust library for using (UNIX domain) sockets for simple synchronous IPC.

- `Socket::new_socketpair` makes a `socketpair` with the settings you want (`AF_UNIX/SOCK_SEQPACKET/CLOEXEC`), but you can use `FromRawFd` of course
- if you want to poll (using `poll`, `select`, `kqueue`, `epoll`, abstractions like [mio](https://github.com/carllerche/mio), etc.), get the fd using `AsRawFd`
- all send/recv methods allow file descriptor (fd) passing
- you can send/recv raw iovecs (scatter-gather vectors), buffers, structs and [serde](https://serde.rs/)-serialized objects
- serde is optional, select a Cargo feature for the format you want (CBOR)

## Usage

```rust
extern crate tiny_nix_ipc;
use tiny_nix_ipc::Socket;
```

Create a socket pair:

```rust
let (mut prnt, mut chld) = Socket::new_socketpair().unwrap();
```

Make a socket non-CLOEXEC (e.g. if you want to fork/exec a different program that should inherit the socket):

```rust
chld.no_cloexec().unwrap();
```

Send bytes:

```rust
let data = [0xDE, 0xAD, 0xBE, 0xEF];
let sent_bytes = prnt.send_slice(&data[..], None).unwrap();
// sent_bytes == 4
```

Receive bytes:

```rust
let mut rdata = [0; 4];
let (recvd_bytes, rfds) = chld.recv_into_slice::<[RawFd; 0]>(&mut rdata[..]).unwrap();
// recvd_bytes == 4, rfds == None
```

Send bytes with a file descriptor ([shmemfdrs](https://github.com/myfreeweb/shmemfdrs) creates an anonymous file, used as an example here, can be any descriptor of course):

```rust
let fd = shmemfdrs::create_shmem(CString::new("/test").unwrap(), 123);
let data = [0xDE, 0xAD, 0xBE, 0xEF];
let sent_bytes = prnt.send_slice(&data[..], Some(&[fd])).unwrap();
// sent_bytes == 4
```

Receive bytes and the file descriptor:

```rust
let mut rdata = [0; 4];
let (recvd_bytes, rfds) = chld.recv_into_slice::<[RawFd; 1]>(&mut rdata[..]).unwrap();
// recvd_bytes == 4, rfds == Some([6])
```

Send a struct, just as its raw bytes (does not work with pointers/references/boxes/etc.!):

```rust
struct TestStruct {
    one: i8,
    two: u32,
}

let data = TestStruct { one: -65, two: 0xDEADBEEF, };
let _ = prnt.send_struct(&data, None).unwrap();
```

Receive a struct:

```rust
let (rdata, rfds) = chld.recv_struct::<TestStruct, [RawFd; 0]>().unwrap();
// rdata == TestStruct { one: -65, two: 0xDEADBEEF, }, rfds == None
```

Send a [Serde](https://serde.rs/)-serializable value serialized as [CBOR](http://cbor.io/) (via [serde_cbor](https://github.com/pyfisch/cbor)):

```toml
tiny-nix-ipc = { version = "0", features = ["cbor"] }
```

```rust
use serde_cbor::value::Value;
let data = Value::U64(123456); // can be your Serializable
let sent_bytes = prnt.send_cbor(&data, None).unwrap();
// sent_bytes == 4
```

Receive a [Serde](https://serde.rs/)-deserializable value serialized as CBOR:

```rust
let (rdata, rfds) = chld.recv_cbor::<Value, [RawFd; 0]>(24).unwrap();
// rdata == Value::U64(123456)
```

## Contributing

Please feel free to submit pull requests!

By participating in this project you agree to follow the [Contributor Code of Conduct](https://www.contributor-covenant.org/version/1/4/).

[The list of contributors is available on GitHub](https://github.com/myfreeweb/tiny-nix-ipc/graphs/contributors).

## License

This is free and unencumbered software released into the public domain.  
For more information, please refer to the `UNLICENSE` file or [unlicense.org](http://unlicense.org).
