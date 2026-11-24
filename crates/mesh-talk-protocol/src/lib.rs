pub mod device;
pub use mesh_replication::session;
pub mod account;
pub mod discovery;
pub mod legacy_event;
pub mod legacy_round;
pub mod legacy_wire;
pub mod mailbox;
fn random_signing_key() -> ed25519_dalek::SigningKey {
    use rand_core::RngCore;
    let mut secret = zeroize::Zeroizing::new([0; 32]);
    rand_core::OsRng.fill_bytes(&mut *secret);
    ed25519_dalek::SigningKey::from_bytes(&secret)
}
