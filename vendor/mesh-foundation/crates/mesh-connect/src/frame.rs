use crate::{MAX_FRAME, TransportError};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Write a length-prefixed frame (4-byte big-endian length + blob) and flush.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    w: &mut W,
    blob: &[u8],
) -> Result<(), TransportError> {
    if blob.len() > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(blob.len()));
    }
    let len = blob.len() as u32;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(blob).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed frame, rejecting absurd lengths before allocating.
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Vec<u8>, TransportError> {
    fn incomplete_frame(error: std::io::Error) -> std::io::Error {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::UnexpectedEof
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::BrokenPipe
            | ErrorKind::NotConnected => std::io::Error::new(ErrorKind::InvalidData, error),
            _ => error,
        }
    }

    let mut len_buf = [0u8; 4];
    // Only a close before any frame bytes is a compatibility/liveness signal.
    // Partial framing is malformed data and must never authorize a downgrade.
    let received = r.read(&mut len_buf).await?;
    if received == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "transport closed at frame boundary",
        )
        .into());
    }
    if received < len_buf.len() {
        r.read_exact(&mut len_buf[received..])
            .await
            .map_err(incomplete_frame)?;
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(TransportError::FrameTooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await.map_err(incomplete_frame)?;
    Ok(buf)
}
