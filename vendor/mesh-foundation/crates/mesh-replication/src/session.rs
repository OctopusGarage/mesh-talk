//! Transport-independent authenticated frame and inventory transfer contracts.

use crate::{CompleteInventory, EventId, InventoryError};

/// The transport owns peer authentication, ordered delivery and the frame
/// length bound. Discard a session after an interrupted inventory exchange.
pub trait ReconciliationSession {
    type Peer;
    type Error;

    fn peer_identity(&self) -> &Self::Peer;
    fn send(&mut self, frame: &[u8]) -> impl Future<Output = Result<(), Self::Error>> + Send;
    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, Self::Error>> + Send;
}

/// Enables direct use of Mesh Connect's pinned-key session with the replication
/// interface. Applications using IP, another carrier, or a custom handshake
/// can instead implement `ReconciliationSession` on their own session type.
#[cfg(feature = "mesh-connect")]
impl<S> ReconciliationSession for mesh_connect::PeerSession<S>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send,
{
    type Peer = [u8; 32];
    type Error = mesh_connect::TransportError;

    fn peer_identity(&self) -> &Self::Peer {
        self.peer_key()
    }

    async fn send(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
        self.send(frame).await
    }

    async fn recv(&mut self) -> Result<Vec<u8>, Self::Error> {
        self.recv().await
    }
}

pub struct HaveLimits {
    pub ids_per_frame: usize,
    pub frames: usize,
    pub total_ids: usize,
}

pub struct HaveFrame<Id> {
    pub ids: Vec<Id>,
    pub more: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum HaveError<E> {
    Exchange(E),
    Limit,
    Allocation,
    CountMismatch,
}

impl<E: std::fmt::Display> std::fmt::Display for HaveError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exchange(error) => write!(f, "inventory exchange failed: {error}"),
            Self::Limit => f.write_str("inventory transfer exceeds its limits"),
            Self::Allocation => f.write_str("unable to allocate bounded inventory"),
            Self::CountMismatch => f.write_str("inventory count differs from the captured view"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for HaveError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Exchange(error) => Some(error),
            _ => None,
        }
    }
}

/// Receive a bounded inventory from framed I/O. The codec checks the protocol
/// variant, conversation and frame contents before returning IDs. This function
/// preserves raw order and duplicates for the caller's admission logic.
pub async fn receive_have<S, Id>(
    session: &mut S,
    limits: HaveLimits,
    decode: impl FnMut(&[u8]) -> Result<HaveFrame<Id>, S::Error>,
) -> Result<Vec<Id>, HaveError<S::Error>>
where
    S: ReconciliationSession,
{
    receive_pages(session, limits, decode, std::convert::identity).await
}

pub(crate) async fn receive_pages<S: ReconciliationSession, Id, Error>(
    session: &mut S,
    limits: HaveLimits,
    mut decode: impl FnMut(&[u8]) -> Result<HaveFrame<Id>, Error>,
    map_transport: impl Fn(S::Error) -> Error,
) -> Result<Vec<Id>, HaveError<Error>> {
    let mut have = Vec::new();
    for _ in 0..limits.frames {
        let bytes = session
            .recv()
            .await
            .map_err(&map_transport)
            .map_err(HaveError::Exchange)?;
        let frame = decode(&bytes).map_err(HaveError::Exchange)?;
        if frame.ids.len() > limits.ids_per_frame {
            return Err(HaveError::Limit);
        }
        let required = have
            .len()
            .checked_add(frame.ids.len())
            .filter(|&count| count <= limits.total_ids)
            .ok_or(HaveError::Limit)?;
        if required > have.capacity() {
            let target = required
                .max(have.capacity().saturating_mul(2))
                .min(limits.total_ids);
            have.try_reserve_exact(target - have.len())
                .map_err(|_| HaveError::Allocation)?;
            if have.capacity() > limits.total_ids {
                return Err(HaveError::Allocation);
            }
        }
        have.extend(frame.ids);
        if !frame.more {
            return Ok(have);
        }
    }
    Err(HaveError::Limit)
}

/// Failure while reading or canonicalizing an inventory stream.
#[derive(Debug, PartialEq, Eq)]
pub enum ReceiveInventoryError<E> {
    Transfer(HaveError<E>),
    Inventory(InventoryError),
}

impl<E: std::fmt::Display> std::fmt::Display for ReceiveInventoryError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transfer(error) => error.fmt(f),
            Self::Inventory(error) => error.fmt(f),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for ReceiveInventoryError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transfer(error) => Some(error),
            Self::Inventory(error) => Some(error),
        }
    }
}

/// Receive a bounded authorized inventory and canonicalize it in place.
///
/// The decoder must validate the stream's role and scope. Only a terminal
/// page completes the inventory. Duplicate IDs fail even across page boundaries.
pub async fn receive_inventory<S: ReconciliationSession>(
    session: &mut S,
    limits: HaveLimits,
    decode: impl FnMut(&[u8]) -> Result<HaveFrame<EventId>, S::Error>,
) -> Result<CompleteInventory, ReceiveInventoryError<S::Error>> {
    let ids = receive_have(session, limits, decode)
        .await
        .map_err(ReceiveInventoryError::Transfer)?;
    CompleteInventory::from_owned(ids).map_err(ReceiveInventoryError::Inventory)
}

/// A finite, already authorized inventory. A page may contain zero IDs and
/// still advance the opaque cursor. The source requalifies authority before
/// every frame is handed to transport.
pub trait HaveSource {
    type Id;
    type Cursor;
    type Error;

    fn page(
        &mut self,
        cursor: Option<&Self::Cursor>,
        ids: usize,
        examined: usize,
    ) -> Result<HavePage<Self::Id, Self::Cursor>, Self::Error>;

    fn validate(&self) -> Result<(), Self::Error>;
}

pub struct HavePage<Id, Cursor> {
    pub ids: Vec<Id>,
    pub next: Option<Cursor>,
}

/// Stream a captured inventory, including an explicit terminal frame. A
/// count disagreement terminates the exchange before publishing a terminal.
pub async fn send_have<S, Source, Yield>(
    session: &mut S,
    source: &mut Source,
    expected_count: usize,
    limits: HaveLimits,
    mut encode: impl FnMut(HaveFrame<Source::Id>) -> Result<Vec<u8>, S::Error>,
    mut yield_work: impl FnMut() -> Yield,
) -> Result<(), HaveError<S::Error>>
where
    S: ReconciliationSession,
    Source: HaveSource<Error = S::Error>,
    Yield: Future<Output = ()>,
{
    if limits.ids_per_frame == 0 || expected_count > limits.total_ids {
        return Err(HaveError::Limit);
    }
    let mut cursor = None;
    let mut total = 0usize;
    let mut frames = 0usize;
    let mut pending = Vec::with_capacity(expected_count.min(limits.ids_per_frame));
    loop {
        let capacity = limits.ids_per_frame - pending.len();
        let budget = capacity.max(1);
        let page = source
            .page(cursor.as_ref(), budget, limits.ids_per_frame)
            .map_err(HaveError::Exchange)?;
        total = total
            .checked_add(page.ids.len())
            .filter(|&n| n <= limits.total_ids)
            .ok_or(HaveError::Limit)?;
        if page.ids.len() > budget {
            return Err(HaveError::Limit);
        }
        cursor = page.next;
        if !page.ids.is_empty() && capacity == 0 {
            frames += 1;
            if frames > limits.frames {
                return Err(HaveError::Limit);
            }
            let bytes = encode(HaveFrame {
                ids: std::mem::take(&mut pending),
                more: true,
            })
            .map_err(HaveError::Exchange)?;
            source.validate().map_err(HaveError::Exchange)?;
            session.send(&bytes).await.map_err(HaveError::Exchange)?;
        }
        pending.reserve_exact(page.ids.len());
        pending.extend(page.ids);
        if cursor.is_none() {
            if total != expected_count {
                return Err(HaveError::CountMismatch);
            }
            if frames == limits.frames {
                return Err(HaveError::Limit);
            }
            let bytes = encode(HaveFrame {
                ids: pending,
                more: false,
            })
            .map_err(HaveError::Exchange)?;
            source.validate().map_err(HaveError::Exchange)?;
            session.send(&bytes).await.map_err(HaveError::Exchange)?;
            return Ok(());
        }
        yield_work().await;
    }
}

/// Limits on one outbound burst. Byte limits are expressed in the caller's
/// wire encoding and in its decoded allocation accounting.
pub struct ExportLimits {
    pub frames: usize,
    pub examined: usize,
    pub events_per_frame: usize,
    pub encoded_per_frame: usize,
    pub allocated_per_frame: usize,
}

#[derive(Clone, Copy)]
pub struct PageSpace {
    pub examined: usize,
    pub events: usize,
    pub encoded_bytes: usize,
    pub allocated_bytes: usize,
}

pub struct EventPage<Event, Cursor> {
    pub events: Vec<Event>,
    pub next: Option<Cursor>,
    pub encoded_bytes: usize,
    pub allocated_bytes: usize,
    pub frame_finished: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub enum ExportError<E> {
    Source(E),
    PageTooSmall(E),
    Limit,
}

pub type ExportResult<Event, Cursor, Error> = Result<EventPage<Event, Cursor>, ExportError<Error>>;

pub trait ExportSource {
    type Event;
    type Cursor;
    type Error;

    /// On PageTooSmall, leave the cursor positioned before the first item
    /// that could not fit so the next frame can retry that row.
    fn page(
        &mut self,
        cursor: &mut Option<Self::Cursor>,
        space: PageSpace,
    ) -> ExportResult<Self::Event, Self::Cursor, Self::Error>;
}

/// Collect a bounded burst without holding source locks across a yield. The
/// caller prepares the source and revalidates it before publishing each frame.
pub async fn select_export_frames<S, Yield>(
    source: &mut S,
    limits: ExportLimits,
    mut yield_work: impl FnMut() -> Yield,
) -> Result<Vec<Vec<S::Event>>, ExportError<S::Error>>
where
    S: ExportSource,
    Yield: Future<Output = ()>,
{
    if limits.frames == 0
        || limits.examined == 0
        || limits.events_per_frame == 0
        || limits.encoded_per_frame == 0
        || limits.allocated_per_frame == 0
    {
        return Err(ExportError::Limit);
    }

    let mut frames = Vec::new();
    let mut cursor = None;
    let mut frame = Vec::new();
    let mut encoded = 0usize;
    let mut allocated = 0usize;
    loop {
        let space = PageSpace {
            examined: limits.examined,
            events: limits
                .events_per_frame
                .checked_sub(frame.len())
                .ok_or(ExportError::Limit)?,
            encoded_bytes: limits
                .encoded_per_frame
                .checked_sub(encoded)
                .ok_or(ExportError::Limit)?,
            allocated_bytes: limits
                .allocated_per_frame
                .checked_sub(allocated)
                .ok_or(ExportError::Limit)?,
        };
        if space.events == 0 || space.encoded_bytes == 0 || space.allocated_bytes == 0 {
            frames.push(std::mem::take(&mut frame));
            if frames.len() == limits.frames {
                return Ok(frames);
            }
            encoded = 0;
            allocated = 0;
            continue;
        }

        let page = match source.page(&mut cursor, space) {
            Ok(page) => page,
            Err(ExportError::PageTooSmall(_)) if !frame.is_empty() => {
                frames.push(std::mem::take(&mut frame));
                if frames.len() == limits.frames {
                    return Ok(frames);
                }
                encoded = 0;
                allocated = 0;
                yield_work().await;
                continue;
            }
            Err(error) => return Err(error),
        };

        encoded = encoded
            .checked_add(page.encoded_bytes)
            .ok_or(ExportError::Limit)?;
        allocated = allocated
            .checked_add(page.allocated_bytes)
            .ok_or(ExportError::Limit)?;
        if page.events.len() > space.events
            || allocated > limits.allocated_per_frame
            || (encoded > limits.encoded_per_frame && !page.frame_finished)
        {
            return Err(ExportError::Limit);
        }
        cursor = page.next;
        frame.extend(page.events);
        let eof = cursor.is_none();
        if !frame.is_empty() && (page.frame_finished || eof) {
            frames.push(std::mem::take(&mut frame));
            if frames.len() == limits.frames {
                return Ok(frames);
            }
            encoded = 0;
            allocated = 0;
        }
        if eof {
            return Ok(frames);
        }
        yield_work().await;
    }
}
