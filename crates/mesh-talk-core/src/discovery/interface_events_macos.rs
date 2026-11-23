//! Darwin route notifications invalidate the local discovery inventory.

use socket2::{Domain, Socket, Type};
use std::{io, os::fd::OwnedFd};
use tokio::io::{Interest, unix::AsyncFd};

pub(super) struct InterfaceEvents(AsyncFd<OwnedFd>);

impl InterfaceEvents {
    pub(super) fn subscribe() -> io::Result<Self> {
        let socket = Socket::new(Domain::from(libc::AF_ROUTE), Type::RAW, None)?;
        socket.set_nonblocking(true)?;
        // SAFETY: The owned, nonblocking descriptor remains open and unchanged
        // until the reactor registration is dropped.
        let registered =
            unsafe { AsyncFd::register_with_interest(OwnedFd::from(socket), Interest::READABLE)? };
        Ok(Self(registered))
    }

    pub(super) async fn next(&self) -> io::Result<()> {
        loop {
            let relevant = self
                .0
                .async_io(Interest::READABLE, |fd| {
                    use std::os::fd::AsRawFd;
                    let mut relevant = false;
                    for _ in 0..32 {
                        // The first four bytes contain the common Darwin route
                        // header. The rest of this datagram is unnecessary.
                        let mut header = [0u8; 4];
                        // SAFETY: header is writable for the duration of recv.
                        let read = unsafe {
                            libc::recv(fd.as_raw_fd(), header.as_mut_ptr().cast(), header.len(), 0)
                        };
                        if read > 0 {
                            relevant |= changes_inventory(&header[..read as usize]);
                            continue;
                        }
                        if read == 0 {
                            return Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "routing socket closed",
                            ));
                        }
                        let error = io::Error::last_os_error();
                        match error.kind() {
                            io::ErrorKind::Interrupted => continue,
                            io::ErrorKind::WouldBlock if relevant => return Ok(true),
                            _ if error.raw_os_error() == Some(libc::ENOBUFS) => return Ok(true),
                            _ => return Err(error),
                        }
                    }
                    Ok(relevant)
                })
                .await?;
            if relevant {
                return Ok(());
            }
            // A burst of unrelated route messages must neither block the
            // executor nor clear readiness while datagrams are still queued.
            tokio::task::yield_now().await;
        }
    }
}

fn changes_inventory(header: &[u8]) -> bool {
    match header {
        [_, _, version, kind] if i32::from(*version) == libc::RTM_VERSION => matches!(
            i32::from(*kind),
            libc::RTM_NEWADDR | libc::RTM_DELADDR | libc::RTM_IFINFO | libc::RTM_IFINFO2
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interface_events_filter_unrelated_route_messages() {
        let header = |kind| [4, 0, libc::RTM_VERSION as u8, kind as u8];
        assert!(changes_inventory(&header(libc::RTM_NEWADDR)));
        assert!(changes_inventory(&header(libc::RTM_DELADDR)));
        assert!(changes_inventory(&header(libc::RTM_IFINFO)));
        assert!(!changes_inventory(&header(libc::RTM_GET)));
        assert!(!changes_inventory(&[4, 0, libc::RTM_VERSION as u8]));
    }

    #[tokio::test]
    async fn interface_events_drain_unrelated_bursts_without_losing_readiness() {
        let (sender, receiver) = std::os::unix::net::UnixDatagram::pair().unwrap();
        socket2::SockRef::from(&receiver)
            .set_recv_buffer_size(64 * 1024)
            .unwrap();
        socket2::SockRef::from(&sender)
            .set_send_buffer_size(64 * 1024)
            .unwrap();
        receiver.set_nonblocking(true).unwrap();
        // SAFETY: This test owns the nonblocking socket for its registration.
        let events = InterfaceEvents(unsafe {
            AsyncFd::register_with_interest(OwnedFd::from(receiver), Interest::READABLE).unwrap()
        });
        let header = |kind| [4, 0, libc::RTM_VERSION as u8, kind as u8];
        for _ in 0..65 {
            sender.send(&header(libc::RTM_GET)).unwrap();
        }
        sender.send(&header(libc::RTM_NEWADDR)).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), events.next())
            .await
            .expect("address event must be observed behind the unrelated burst")
            .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), events.next())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn interface_events_open_the_native_routing_socket() {
        let events = InterfaceEvents::subscribe().unwrap();
        use std::os::fd::AsRawFd;
        let fd = events.0.get_ref().as_raw_fd();
        // SAFETY: The subscription owns the open descriptor.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0 && flags & libc::O_NONBLOCK != 0);
        if let Ok(event) =
            tokio::time::timeout(std::time::Duration::from_millis(10), events.next()).await
        {
            event.unwrap();
        }
    }
}
