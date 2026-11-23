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
}

struct Observation {
    id: u64,
    expires: Instant,
    sent: u64,
    received: u64,
}

pub fn begin() -> u64 {
    let mut observation = OBSERVATION.lock().unwrap();
    let id = SEQUENCE.fetch_add(1, Relaxed).wrapping_add(1);
    *observation = Some(Observation {
        id,
        expires: Instant::now() + LEASE,
        sent: 0,
        received: 0,
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
