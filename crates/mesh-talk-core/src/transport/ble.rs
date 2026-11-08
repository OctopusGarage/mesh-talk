//! Bounded, session-scoped byte stream for native BLE GATT adapters.
use std::{
    io,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    sync::mpsc,
};

pub const MAX_BLE_CHUNK: usize = 512;
pub const MAX_BLE_QUEUE: usize = 64;

/// Session byte stream. Flush confirms bounded queue admission, not radio delivery.
type Reservation = Pin<
    Box<
        dyn std::future::Future<
                Output = Result<mpsc::OwnedPermit<Vec<u8>>, mpsc::error::SendError<()>>,
            > + Send,
    >,
>;

pub struct BleStream {
    closed: Arc<AtomicBool>,
    inbound: mpsc::Receiver<Vec<u8>>,
    outbound: Option<mpsc::Sender<Vec<u8>>>,
    reservation: Option<Reservation>,
    pending: Option<Vec<u8>>,
    offset: usize,
    max_chunk: usize,
}

/// Native callbacks must retain only this endpoint for their admitted session.
pub struct BleEndpoint {
    closed: Arc<AtomicBool>,
    inbound: mpsc::Sender<Vec<u8>>,
    outbound: mpsc::Receiver<Vec<u8>>,
    max_chunk: usize,
}

impl Drop for BleStream {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

impl BleStream {
    pub fn new(max_chunk: usize, queue_capacity: usize) -> io::Result<(Self, BleEndpoint)> {
        if !(1..=MAX_BLE_CHUNK).contains(&max_chunk)
            || !(1..=MAX_BLE_QUEUE).contains(&queue_capacity)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid BLE buffer budget",
            ));
        }
        let (incoming, inbound) = mpsc::channel(queue_capacity);
        let (outbound, outgoing) = mpsc::channel(queue_capacity);
        let closed = Arc::new(AtomicBool::new(false));
        Ok((
            Self {
                closed: closed.clone(),
                inbound,
                outbound: Some(outbound),
                reservation: None,
                pending: None,
                offset: 0,
                max_chunk,
            },
            BleEndpoint {
                closed,
                inbound: incoming,
                outbound: outgoing,
                max_chunk,
            },
        ))
    }
}
impl Drop for BleEndpoint {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
    }
}

impl BleEndpoint {
    pub fn try_send(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > self.max_chunk {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid BLE chunk size",
            ));
        }
        let permit = self.inbound.try_reserve().map_err(|e| match e {
            mpsc::error::TrySendError::Full(_) => {
                io::Error::new(io::ErrorKind::WouldBlock, "BLE queue full")
            }
            mpsc::error::TrySendError::Closed(_) => {
                io::Error::new(io::ErrorKind::BrokenPipe, "BLE session closed")
            }
        })?;
        permit.send(bytes.to_vec());
        Ok(())
    }
    pub async fn recv(&mut self) -> Option<Vec<u8>> {
        if self.closed.load(Ordering::Acquire) {
            self.outbound.close();
            while self.outbound.try_recv().is_ok() {}
            return None;
        }
        let chunk = self.outbound.recv().await;
        if self.closed.load(Ordering::Acquire) {
            None
        } else {
            chunk
        }
    }
}
impl AsyncRead for BleStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.closed.load(Ordering::Acquire) {
            this.pending = None;
            while this.inbound.try_recv().is_ok() {}
            return Poll::Ready(Ok(()));
        }
        if buf.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if this.pending.is_none() {
            match this.inbound.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(Ok(())),
                Poll::Ready(Some(chunk)) => {
                    this.pending = Some(chunk);
                    this.offset = 0;
                }
            }
        }
        let chunk = this.pending.as_ref().unwrap();
        let count = buf.remaining().min(chunk.len() - this.offset);
        buf.put_slice(&chunk[this.offset..this.offset + count]);
        this.offset += count;
        if this.offset == chunk.len() {
            this.pending = None;
        }
        Poll::Ready(Ok(()))
    }
}
impl AsyncWrite for BleStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.closed.load(Ordering::Acquire) {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        let Some(sender) = this.outbound.as_ref() else {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        };
        if this.reservation.is_none() {
            this.reservation = Some(Box::pin(sender.clone().reserve_owned()));
        }
        match this.reservation.as_mut().unwrap().as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(result) => {
                this.reservation = None;
                match result {
                    Err(_) => Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
                    Ok(permit) => {
                        let n = bytes.len().min(this.max_chunk);
                        permit.send(bytes[..n].to_vec());
                        Poll::Ready(Ok(n))
                    }
                }
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.closed.store(true, Ordering::Release);
        this.reservation = None;
        this.outbound = None;
        this.inbound.close();
        this.pending = None;
        while this.inbound.try_recv().is_ok() {}
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn chunk_order_partial_read_and_limits() {
        assert!(BleStream::new(MAX_BLE_CHUNK + 1, 1).is_err());
        assert!(BleStream::new(1, MAX_BLE_QUEUE + 1).is_err());
        let (mut stream, endpoint) = BleStream::new(3, 2).unwrap();
        assert_eq!(
            endpoint.try_send(b"long").unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        endpoint.try_send(b"abc").unwrap();
        endpoint.try_send(b"de").unwrap();
        assert_eq!(
            endpoint.try_send(b"f").unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        let mut bytes = [0; 5];
        stream.read_exact(&mut bytes[..1]).await.unwrap();
        stream.read_exact(&mut bytes[1..]).await.unwrap();
        assert_eq!(&bytes, b"abcde");
    }

    #[tokio::test]
    async fn saturation_resumes_and_disconnect_closes() {
        let (mut stream, mut endpoint) = BleStream::new(3, 1).unwrap();
        stream.write_all(b"abc").await.unwrap();
        let write = stream.write_all(b"def");
        tokio::pin!(write);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut write)
                .await
                .is_err()
        );
        assert_eq!(endpoint.recv().await.unwrap(), b"abc");
        write.await.unwrap();
        drop(endpoint);
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        assert_eq!(
            stream.write(b"x").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn shutdown_revokes_buffered_chunks_and_endpoint() {
        let (mut stream, mut endpoint) = BleStream::new(3, 2).unwrap();
        endpoint.try_send(b"abc").unwrap();
        endpoint.try_send(b"def").unwrap();
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        stream.write_all(b"abc").await.unwrap();
        stream.shutdown().await.unwrap();
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        assert!(endpoint.recv().await.is_none());
        assert_eq!(
            endpoint.try_send(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            stream.write(b"x").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn drop_revokes_buffered_outbound() {
        let (mut stream, mut endpoint) = BleStream::new(3, 2).unwrap();
        stream.write_all(b"abc").await.unwrap();
        drop(stream);
        assert!(endpoint.recv().await.is_none());
        assert_eq!(
            endpoint.try_send(b"x").unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn endpoint_drop_revokes_pending_and_queued_input() {
        let (mut stream, endpoint) = BleStream::new(3, 2).unwrap();
        endpoint.try_send(b"abc").unwrap();
        endpoint.try_send(b"def").unwrap();
        let mut byte = [0];
        stream.read_exact(&mut byte).await.unwrap();
        drop(endpoint);
        assert_eq!(stream.read(&mut byte).await.unwrap(), 0);
        assert_eq!(
            stream.write(b"x").await.unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn authenticated_encrypted_round_trip() {
        use crate::{identity::device::DeviceIdentity, transport::SecureChannel};
        let (left, mut a) = BleStream::new(32, 64).unwrap();
        let (right, mut b) = BleStream::new(32, 64).unwrap();
        let pump = tokio::spawn(async move {
            loop {
                tokio::select! {
                    chunk = a.recv() => match chunk { Some(chunk) => b.try_send(&chunk).unwrap(), None => break },
                    chunk = b.recv() => match chunk { Some(chunk) => a.try_send(&chunk).unwrap(), None => break },
                }
            }
        });
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let bob_public = bob.public();
        let exchange = async {
            let (a, b) = tokio::join!(
                SecureChannel::connect(left, &alice, Some(&bob_public)),
                SecureChannel::accept(right, &bob)
            );
            let mut a = a.unwrap();
            let mut b = b.unwrap();
            a.send(b"encrypted over BLE").await.unwrap();
            assert_eq!(b.recv().await.unwrap(), b"encrypted over BLE");
            b.send(b"ack").await.unwrap();
            assert_eq!(a.recv().await.unwrap(), b"ack");
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), exchange).await;
        pump.abort();
        result.unwrap();
    }
}
