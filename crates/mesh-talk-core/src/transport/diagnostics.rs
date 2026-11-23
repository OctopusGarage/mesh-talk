//! Process-local counters for the opt-in desktop resource monitor.
use std::sync::{
    Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering::Relaxed},
};
use std::time::{Duration, Instant};

const LEASE: Duration = Duration::from_secs(5);
static CHANNELS: AtomicU64 = AtomicU64::new(0);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
static ENABLED: AtomicBool = AtomicBool::new(false);
static OBSERVATION: Mutex<Option<Observation>> = Mutex::new(None);

#[derive(Clone, Copy)]
pub struct TransportCounters {
    pub authenticated_channels: u64,
    pub sent_frame_bytes: u64,
    pub received_frame_bytes: u64,
    pub messages: MessageCounters,
}

struct Observation {
    id: u64,
    expires: Instant,
    sent: u64,
    received: u64,
    messages: MessageCounters,
}

pub fn begin() -> u64 {
    let mut observation = OBSERVATION.lock().unwrap();
    let id = SEQUENCE.fetch_add(1, Relaxed).wrapping_add(1);
    *observation = Some(Observation {
        id,
        expires: Instant::now() + LEASE,
        sent: 0,
        received: 0,
        messages: MessageCounters::default(),
    });
    ENABLED.store(true, Relaxed);
    id
}

pub fn stop(id: u64) {
    let mut observation = OBSERVATION.lock().unwrap();
    if observation.as_ref().is_some_and(|value| value.id == id) {
        *observation = None;
        ENABLED.store(false, Relaxed);
    }
}

/// Retire the desktop observation at the owner or app-lock boundary.
pub fn stop_current() {
    let mut observation = OBSERVATION.lock().unwrap();
    *observation = None;
    ENABLED.store(false, Relaxed);
}

pub fn sample(id: u64) -> Option<TransportCounters> {
    let mut observation = OBSERVATION.lock().unwrap();
    let value = observation.as_mut().filter(|value| value.id == id)?;
    let now = Instant::now();
    if now >= value.expires {
        *observation = None;
        ENABLED.store(false, Relaxed);
        return None;
    }
    value.expires = now + LEASE;
    Some(TransportCounters {
        authenticated_channels: CHANNELS.load(Relaxed),
        sent_frame_bytes: value.sent,
        received_frame_bytes: value.received,
        messages: value.messages,
    })
}

pub(super) fn frame(sent: bool, bytes: usize) {
    if !ENABLED.load(Relaxed) {
        return;
    }
    let mut observation = OBSERVATION.lock().unwrap();
    let Some(value) = observation.as_mut() else {
        return;
    };
    if Instant::now() >= value.expires {
        ENABLED.store(false, Relaxed);
        return;
    }
    let total = if sent {
        &mut value.sent
    } else {
        &mut value.received
    };
    *total = total.saturating_add(bytes as u64);
}

/// Fixed counters and latency buckets. Never retain message identifiers or payloads.
#[derive(Clone, Copy, Default, serde::Serialize)]
pub struct MessageCounters {
    pub accepted: u64,
    pub delivered: u64,
    pub rejected: u64,
    pub send_request_errors: u64,
    pub route_restarts: u64,
    pub direct_storage_holds: u64,
    pub relay_storage_holds: u64,
    pub attachments_accepted: u64,
    pub attachments_declined: u64,
    pub attempt_timeouts: u64,
    pub retries_scheduled: u64,
    pub received_direct: [MessageVolume; 4],
    pub received_group: [MessageVolume; 4],
    pub catch_up: u64,
    pub history_sync: u64,
    pub own_device_sync: u64,
    pub duplicate_direct_events: u64,
    pub attachments_verified: u64,
    pub verified_attachment_bytes: u64,
    delivery_latency: [u64; 8],
}

#[derive(Clone, Copy, Default, serde::Serialize)]
pub struct MessageVolume {
    pub count: u64,
    pub bytes: u64,
}

#[derive(Clone, Copy)]
pub(crate) enum ContentKind {
    Text,
    Sticker,
    Media,
    Attachment,
}

impl MessageCounters {
    pub(crate) fn received(&mut self, group: bool, kind: ContentKind, bytes: u64) {
        let buckets = if group {
            &mut self.received_group
        } else {
            &mut self.received_direct
        };
        let bucket = &mut buckets[kind as usize];
        bucket.count = bucket.count.saturating_add(1);
        bucket.bytes = bucket.bytes.saturating_add(bytes);
    }

    pub(crate) fn delivered(&mut self, elapsed_ms: Option<u64>) {
        self.delivered = self.delivered.saturating_add(1);
        if let Some(elapsed_ms) = elapsed_ms {
            let index =
                [50, 100, 250, 500, 1000, 3000, 10000].partition_point(|bound| *bound < elapsed_ms);
            self.delivery_latency[index] = self.delivery_latency[index].saturating_add(1);
        }
    }
}

pub(crate) fn observing_messages() -> bool {
    ENABLED.load(Relaxed)
}

pub(crate) fn record_message(record: impl FnOnce(&mut MessageCounters)) {
    if !observing_messages() {
        return;
    }
    let mut observation = OBSERVATION.lock().unwrap();
    if let Some(value) = observation.as_mut()
        && Instant::now() < value.expires
    {
        record(&mut value.messages);
    }
}

pub(crate) fn note_duplicate_receive() {
    record_message(|metrics| {
        metrics.duplicate_direct_events = metrics.duplicate_direct_events.saturating_add(1);
    });
}

/// Desktop command errors before or during acceptance. A failed outbox write
/// can also appear here if it was triggered by a desktop send command.
pub fn note_send_request_error() {
    record_message(|metrics| {
        metrics.send_request_errors = metrics.send_request_errors.saturating_add(1);
    });
}

pub(super) struct ChannelLifetime;

impl ChannelLifetime {
    pub(super) fn new() -> Self {
        CHANNELS.fetch_add(1, Relaxed);
        Self
    }
}

impl Drop for ChannelLifetime {
    fn drop(&mut self) {
        CHANNELS.fetch_sub(1, Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_message_buckets_keep_direction_and_size_separate() {
        let mut counters = MessageCounters::default();
        counters.received(false, ContentKind::Text, 5);
        counters.received(false, ContentKind::Text, 7);
        counters.received(true, ContentKind::Attachment, 1024);
        counters.delivered(Some(50));
        counters.delivered(Some(51));
        counters.delivered(None);
        assert_eq!(counters.received_direct[0].count, 2);
        assert_eq!(counters.received_direct[0].bytes, 12);
        assert_eq!(counters.received_direct[3].count, 0);
        assert_eq!(counters.received_group[3].bytes, 1024);
        assert_eq!(counters.delivered, 3);
        assert_eq!(counters.delivery_latency, [1, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn encrypted_loopback_is_counted_only_during_a_current_observation() {
        use crate::{identity::device::DeviceIdentity, transport::channel::SecureChannel};
        use tokio::net::{TcpListener, TcpStream};

        let original = begin();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let initiator = DeviceIdentity::generate();
        let responder = DeviceIdentity::generate();
        let expected = responder.public();
        let (mut client, mut server) = tokio::join!(
            async {
                let socket = TcpStream::connect(addr).await.unwrap();
                SecureChannel::connect(socket, &initiator, Some(&expected))
                    .await
                    .unwrap()
            },
            async {
                let (socket, _) = listener.accept().await.unwrap();
                SecureChannel::accept(socket, &responder).await.unwrap()
            }
        );

        let before = sample(original).unwrap();
        assert!(before.authenticated_channels >= 2);
        client.send(b"ping").await.unwrap();
        assert_eq!(server.recv().await.unwrap(), b"ping");
        let after = sample(original).unwrap();
        // Other transport tests can run alongside this process-wide observer.
        assert!(after.sent_frame_bytes - before.sent_frame_bytes >= 24);
        assert!(after.received_frame_bytes - before.received_frame_bytes >= 24);
        drop((client, server));

        let current = begin();
        assert!(sample(original).is_none());
        stop(original);
        assert!(sample(current).is_some());
        OBSERVATION.lock().unwrap().as_mut().unwrap().expires = Instant::now();
        frame(true, 100);
        assert!(sample(current).is_none());
        assert!(!ENABLED.load(Relaxed));
    }
}
