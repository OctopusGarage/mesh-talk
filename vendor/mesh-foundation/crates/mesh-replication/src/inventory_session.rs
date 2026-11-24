//! Default bounded inventory frames for one authenticated record scope.

use crate::{
    CompleteInventory, EventId,
    pull::MAX_RECORD_FRAME,
    session::{
        HaveError, HaveFrame, HaveLimits, ReceiveInventoryError, ReconciliationSession,
        receive_pages,
    },
};

const MAGIC: &[u8; 4] = b"MIV1";
const HEADER: usize = MAGIC.len() + 32 + 2;

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum InventorySender {
    Requester = 1,
    Responder = 2,
}

#[derive(Debug, PartialEq, Eq)]
pub enum InventoryFrameError<E> {
    Transport(E),
    InvalidFrame,
    WrongScopeOrSender,
}

impl<E: std::fmt::Display> std::fmt::Display for InventoryFrameError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "inventory session failed: {error}"),
            Self::InvalidFrame => f.write_str("invalid inventory frame"),
            Self::WrongScopeOrSender => f.write_str("inventory scope or sender does not match"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for InventoryFrameError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

/// Send a finite canonical inventory. Recheck authority before each page.
/// Failed exchanges leave the session unsuitable for continuing the protocol.
pub async fn send_inventory<S: ReconciliationSession>(
    session: &mut S,
    scope: [u8; 32],
    sender: InventorySender,
    inventory: &CompleteInventory,
    limits: HaveLimits,
    mut authorize: impl FnMut() -> Result<(), S::Error>,
) -> Result<(), HaveError<S::Error>> {
    if !valid_limits(&limits)
        || inventory.len() > limits.total_ids
        || inventory.len().div_ceil(limits.ids_per_frame).max(1) > limits.frames
    {
        return Err(HaveError::Limit);
    }
    let mut pages = inventory.ids.chunks(limits.ids_per_frame).peekable();
    let capacity = HEADER + inventory.len().min(limits.ids_per_frame) * 32;
    let mut bytes = Vec::with_capacity(capacity);
    loop {
        let ids = pages.next().unwrap_or_default();
        let more = pages.peek().is_some();
        authorize().map_err(HaveError::Exchange)?;
        bytes.clear();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&scope);
        bytes.extend_from_slice(&[sender as u8, u8::from(more)]);
        bytes.extend_from_slice(ids.as_flattened());
        session.send(&bytes).await.map_err(HaveError::Exchange)?;
        if !more {
            return Ok(());
        }
    }
}

/// Receive the default inventory format, enforcing sender and scope before
/// collecting IDs. Only the terminal page completes an inventory.
pub async fn receive_inventory<S: ReconciliationSession>(
    session: &mut S,
    scope: [u8; 32],
    sender: InventorySender,
    limits: HaveLimits,
) -> Result<CompleteInventory, ReceiveInventoryError<InventoryFrameError<S::Error>>> {
    if !valid_limits(&limits) {
        return Err(ReceiveInventoryError::Transfer(HaveError::Limit));
    }
    let page_limit = limits.ids_per_frame;
    let ids = receive_pages(
        session,
        limits,
        |bytes| decode(bytes, &scope, sender, page_limit),
        InventoryFrameError::Transport,
    )
    .await
    .map_err(ReceiveInventoryError::Transfer)?;
    CompleteInventory::from_owned(ids).map_err(ReceiveInventoryError::Inventory)
}

fn valid_limits(limits: &HaveLimits) -> bool {
    limits.frames != 0 && (1..=(MAX_RECORD_FRAME - HEADER) / 32).contains(&limits.ids_per_frame)
}

fn decode<E>(
    bytes: &[u8],
    scope: &[u8; 32],
    sender: InventorySender,
    page_limit: usize,
) -> Result<HaveFrame<EventId>, InventoryFrameError<E>> {
    if !(HEADER..=MAX_RECORD_FRAME).contains(&bytes.len()) || !bytes.starts_with(MAGIC) {
        return Err(InventoryFrameError::InvalidFrame);
    }
    if &bytes[4..36] != scope || bytes[36] != sender as u8 {
        return Err(InventoryFrameError::WrongScopeOrSender);
    }
    let (ids, trailing) = bytes[HEADER..].as_chunks::<32>();
    let more = bytes[37];
    if !trailing.is_empty() || ids.len() > page_limit || more > 1 || (more == 1 && ids.is_empty()) {
        return Err(InventoryFrameError::InvalidFrame);
    }
    Ok(HaveFrame {
        ids: ids.to_vec(),
        more: more == 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[derive(Default)]
    struct Frames(VecDeque<Vec<u8>>);

    impl ReconciliationSession for Frames {
        type Peer = ();
        type Error = &'static str;

        fn peer_identity(&self) -> &Self::Peer {
            &()
        }

        async fn send(&mut self, frame: &[u8]) -> Result<(), Self::Error> {
            self.0.push_back(frame.to_vec());
            Ok(())
        }

        async fn recv(&mut self) -> Result<Vec<u8>, Self::Error> {
            self.0.pop_front().ok_or("closed")
        }
    }

    fn limits() -> HaveLimits {
        HaveLimits {
            ids_per_frame: 1,
            frames: 2,
            total_ids: 2,
        }
    }

    #[tokio::test]
    async fn default_inventory_checks_every_page_and_sender() {
        let source = CompleteInventory::from_ids(&[[2; 32], [1; 32]], 2).unwrap();
        let mut frames = Frames::default();
        let mut checks = 0;
        send_inventory(
            &mut frames,
            [7; 32],
            InventorySender::Responder,
            &source,
            limits(),
            || {
                checks += 1;
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!(checks, 2);
        let mut wrong = Frames(frames.0.clone());
        assert!(matches!(
            receive_inventory(&mut wrong, [7; 32], InventorySender::Requester, limits()).await,
            Err(ReceiveInventoryError::Transfer(HaveError::Exchange(
                InventoryFrameError::WrongScopeOrSender
            )))
        ));
        let actual = receive_inventory(&mut frames, [7; 32], InventorySender::Responder, limits())
            .await
            .unwrap();
        assert_eq!(actual.fingerprint(), source.fingerprint());
    }

    #[tokio::test]
    async fn short_terminal_page_has_no_leftover_ids_and_denial_stops_the_exchange() {
        let scope = [7; 32];
        let sender = InventorySender::Responder;
        let limits = || HaveLimits {
            ids_per_frame: 2,
            frames: 2,
            total_ids: 3,
        };
        let source = CompleteInventory::from_ids(&[[1; 32], [2; 32], [3; 32]], 3).unwrap();
        let mut frames = Frames::default();
        send_inventory(&mut frames, scope, sender, &source, limits(), || Ok(()))
            .await
            .unwrap();
        let [first, last] = frames.0.make_contiguous() else {
            panic!("expected two pages");
        };
        assert_eq!(first.len(), HEADER + 64);
        assert_eq!(last.len(), HEADER + 32);
        let terminal = decode::<&str>(last, &scope, sender, 2).unwrap();
        assert_eq!(terminal.ids, [[3; 32]]);
        assert!(!terminal.more);

        let mut frames = Frames::default();
        let mut authorized = false;
        let result = send_inventory(&mut frames, scope, sender, &source, limits(), || {
            if std::mem::replace(&mut authorized, true) {
                Err("revoked")
            } else {
                Ok(())
            }
        })
        .await;
        assert!(matches!(result, Err(HaveError::Exchange("revoked"))));
        assert_eq!(frames.0.len(), 1);
    }
}
