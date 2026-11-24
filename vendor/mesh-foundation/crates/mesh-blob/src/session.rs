//! Single-object chunk exchange over an authenticated mesh-connect session.
use crate::ChunkSource;
use mesh_connect::{MAX_PLAINTEXT, PeerSession, TransportError};
use std::{io, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::Mutex,
    time::timeout,
};

const AVAILABILITY: &[u8; 4] = b"MBA1";
pub const MAX_AVAILABILITY_PAGE: u32 = 4096;
const REQUEST: &[u8; 4] = b"MBC1";
const BOUNDED_REQUEST: &[u8; 4] = b"MBR1";
const DATA: u8 = 1;
const MISSING: u8 = 0;
pub const MAX_CHUNK_BODY: usize = MAX_PLAINTEXT - 1;

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid blob chunk exchange")
}

/// Ordered authenticated frames. A host can use its existing secure channel.
pub trait BlobSession {
    fn send(&mut self, frame: &[u8]) -> impl Future<Output = Result<(), TransportError>>;
    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, TransportError>>;
}

impl<S: AsyncRead + AsyncWrite + Unpin> BlobSession for PeerSession<S> {
    async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError> {
        PeerSession::send(self, frame).await
    }

    async fn recv(&mut self) -> Result<Vec<u8>, TransportError> {
        PeerSession::recv(self).await
    }
}

pub type SessionChunkSource<S> = AuthenticatedChunkSource<PeerSession<S>>;

/// One peer's availability snapshot and reusable authenticated connection.
/// The host performs discovery, verifies the peer and updates availability
/// by constructing a new source. This adapter returns the wire body as-is.
pub struct AuthenticatedChunkSource<S> {
    object: [u8; 32],
    available: Vec<bool>,
    session: Mutex<Option<S>>,
    deadline: Duration,
}

impl<S> AuthenticatedChunkSource<S> {
    pub fn new(object: [u8; 32], available: Vec<bool>, session: S, deadline: Duration) -> Self {
        Self {
            object,
            available,
            session: Mutex::new(Some(session)),
            deadline,
        }
    }
}

impl<S: BlobSession> AuthenticatedChunkSource<S> {
    /// Obtain a bounded availability snapshot on the authenticated connection.
    /// The caller supplies the validated manifest count and its local budget.
    pub async fn discover(
        object: [u8; 32],
        count: u32,
        max_chunks: u32,
        mut session: S,
        deadline: Duration,
    ) -> Result<Self, TransportError> {
        if count == 0 || count > max_chunks {
            return Err(invalid().into());
        }
        let available = timeout(deadline, async {
            let mut available = Vec::new();
            available
                .try_reserve_exact(count as usize)
                .map_err(|_| invalid())?;
            let mut start = 0;
            while start < count {
                let length = (count - start).min(MAX_AVAILABILITY_PAGE);
                let request = encode_request(AVAILABILITY, object, start, length);
                session.send(&request).await?;
                let response = session.recv().await?;
                let Some((&DATA, page)) = response.split_first() else {
                    return Err(io::Error::from(io::ErrorKind::NotFound).into());
                };
                if page.len() != length as usize || page.iter().any(|&value| value > 1) {
                    return Err(invalid().into());
                }
                available.extend(page.iter().map(|&value| value == 1));
                start += length;
            }
            Ok::<_, TransportError>(available)
        })
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        Ok(Self::new(object, available, session, deadline))
    }
}

fn encode_request(tag: &[u8; 4], object: [u8; 32], index: u32, length: u32) -> [u8; 44] {
    let mut request = [0; 44];
    request[..4].copy_from_slice(tag);
    request[4..36].copy_from_slice(&object);
    request[36..40].copy_from_slice(&index.to_be_bytes());
    request[40..].copy_from_slice(&length.to_be_bytes());
    request
}

impl<S: BlobSession> ChunkSource for AuthenticatedChunkSource<S> {
    type Error = TransportError;

    fn has_chunk(&self, index: u32) -> bool {
        self.available.get(index as usize).copied().unwrap_or(false)
    }

    async fn fetch(&self, index: u32, length: u32) -> Result<Vec<u8>, TransportError> {
        self.request(REQUEST, index, length).await
    }
}

impl<S: BlobSession> AuthenticatedChunkSource<S> {
    /// Fetch a variable-length wrapped block, such as a signed event. The host
    /// validates the envelope and applies it through its normal storage path.
    pub async fn fetch_bounded(&self, index: u32, limit: u32) -> Result<Vec<u8>, TransportError> {
        self.request(BOUNDED_REQUEST, index, limit).await
    }

    async fn request(
        &self,
        tag: &[u8; 4],
        index: u32,
        length: u32,
    ) -> Result<Vec<u8>, TransportError> {
        if length as usize > MAX_CHUNK_BODY {
            return Err(invalid().into());
        }
        timeout(self.deadline, async {
            let mut slot = self.session.lock().await;
            let mut session = slot
                .take()
                .ok_or(io::Error::from(io::ErrorKind::NotConnected))?;
            let request = encode_request(tag, self.object, index, length);
            session.send(&request).await?;
            let reply = session.recv().await?;
            let result = match reply.split_first() {
                Some((&DATA, body)) if response_length_matches(tag, body.len(), length) => {
                    Ok(body.to_vec())
                }
                Some((&MISSING, [])) => Err(io::Error::from(io::ErrorKind::NotFound).into()),
                _ => return Err(invalid().into()),
            };
            *slot = Some(session);
            result
        })
        .await
        .map_err(|_| io::Error::from(io::ErrorKind::TimedOut))?
    }
}

/// Serve one request. The loader must authorize the authenticated peer for
/// every object and index before reading. Return None for unavailable data.
pub async fn serve_chunk<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut PeerSession<S>,
    load: impl FnOnce(&[u8; 32], [u8; 32], u32, u32) -> Option<Vec<u8>>,
) -> Result<(), TransportError> {
    serve_request(session, load, |_, _, _, _| None).await
}

/// Dispatch one request from either an app or a relay. Both callbacks must
/// recheck authorization. Return None without revealing object availability.
pub async fn serve_request<S: AsyncRead + AsyncWrite + Unpin>(
    session: &mut PeerSession<S>,
    load: impl FnOnce(&[u8; 32], [u8; 32], u32, u32) -> Option<Vec<u8>>,
    availability: impl FnOnce(&[u8; 32], [u8; 32], u32, u32) -> Option<Vec<bool>>,
) -> Result<(), TransportError> {
    let bytes = session.recv().await?;
    let reply = respond_to_request(&bytes, session.peer_key(), load, availability)?;
    session.send(&reply).await
}

/// Identify a blob frame at the existing host's authenticated dispatch boundary.
pub fn is_blob_request(bytes: &[u8]) -> bool {
    bytes.starts_with(REQUEST)
        || bytes.starts_with(AVAILABILITY)
        || bytes.starts_with(BOUNDED_REQUEST)
}

/// Build a reply for an already authenticated connection. Hosts can keep their
/// existing session type and pass its verified peer identity to both loaders.
pub fn respond_to_request<P>(
    bytes: &[u8],
    peer: &P,
    load: impl FnOnce(&P, [u8; 32], u32, u32) -> Option<Vec<u8>>,
    availability: impl FnOnce(&P, [u8; 32], u32, u32) -> Option<Vec<bool>>,
) -> io::Result<Vec<u8>> {
    let request: [u8; 44] = bytes.try_into().map_err(|_| invalid())?;
    let object = request[4..36].try_into().unwrap();
    let index = u32::from_be_bytes(request[36..40].try_into().unwrap());
    let length = u32::from_be_bytes(request[40..].try_into().unwrap());
    let body = match &request[..4] {
        tag if (tag == REQUEST || tag == BOUNDED_REQUEST) && length as usize <= MAX_CHUNK_BODY => {
            load(peer, object, index, length)
        }
        tag if tag == AVAILABILITY
            && (1..=MAX_AVAILABILITY_PAGE).contains(&length)
            && index.checked_add(length).is_some() =>
        {
            availability(peer, object, index, length)
                .map(|page| page.into_iter().map(u8::from).collect())
        }
        _ => return Err(invalid()),
    };
    if let Some(body) = body {
        if !response_length_matches(&request[..4], body.len(), length) {
            return Err(invalid());
        }
        let mut reply = Vec::with_capacity(body.len() + 1);
        reply.push(DATA);
        reply.extend_from_slice(&body);
        Ok(reply)
    } else {
        Ok(vec![MISSING])
    }
}

fn response_length_matches(tag: &[u8], actual: usize, length: u32) -> bool {
    if tag == BOUNDED_REQUEST {
        actual <= length as usize
    } else {
        actual == length as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_envelope_response_obeys_the_requested_byte_limit() {
        let object = [7; 32];
        let request = encode_request(BOUNDED_REQUEST, object, 1, 4);
        let load = |peer: &u8, id, index, length| {
            assert_eq!((*peer, id, index, length), (9, object, 1, 4));
            Some(vec![2, 3, 4])
        };
        let reply = respond_to_request(&request, &9, load, |_, _, _, _| None).unwrap();
        assert_eq!(reply, [DATA, 2, 3, 4]);
        assert!(
            respond_to_request(
                &request,
                &9,
                |_, _, _, _| Some(vec![0; 5]),
                |_, _, _, _| None
            )
            .is_err()
        );
        let exact = encode_request(REQUEST, object, 1, 4);
        assert!(respond_to_request(&exact, &9, load, |_, _, _, _| None).is_err());
    }
}
