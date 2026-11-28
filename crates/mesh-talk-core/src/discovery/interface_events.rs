//! Linux link and address notifications only invalidate the local inventory.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use tokio::io::{Interest, unix::AsyncFd};

pub(super) struct InterfaceEvents(AsyncFd<OwnedFd>);

impl InterfaceEvents {
    pub(super) fn subscribe() -> io::Result<Self> {
        // SAFETY: socket returns a new owned descriptor on success.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
                libc::NETLINK_ROUTE,
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: raw is a successful socket result, with no other owner.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: the netlink address consists of integer fields and padding.
        let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
        address.nl_groups =
            (libc::RTMGRP_LINK | libc::RTMGRP_IPV4_IFADDR | libc::RTMGRP_IPV6_IFADDR) as u32;
        // SAFETY: the address lives for the call and points to the C layout
        // described by the size provided to bind.
        let result = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&raw const address).cast(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        };
        if result < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the descriptor is owned, nonblocking, and never replaced or
        // closed while registered. Only this reactor reads it.
        let event_socket = unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE)? };
        Ok(Self(event_socket))
    }

    pub(super) async fn next(&self) -> io::Result<()> {
        self.0
            .async_io(Interest::READABLE, |fd| {
                let mut observed = false;
                // Datagram contents are unnecessary. Drop a bounded batch and
                // reconcile against the authoritative interface inventory.
                for _ in 0..32 {
                    let mut byte = 0u8;
                    // SAFETY: the byte is writable for the duration of recv.
                    let result = unsafe {
                        libc::recv(
                            fd.as_raw_fd(),
                            (&raw mut byte).cast(),
                            1,
                            libc::MSG_DONTWAIT,
                        )
                    };
                    if result >= 0 {
                        observed = true;
                        continue;
                    }
                    let error = io::Error::last_os_error();
                    match error.kind() {
                        io::ErrorKind::Interrupted => continue,
                        io::ErrorKind::WouldBlock if observed => return Ok(()),
                        _ if error.raw_os_error() == Some(libc::ENOBUFS) => return Ok(()),
                        _ => return Err(error),
                    }
                }
                Ok(())
            })
            .await
    }
}
