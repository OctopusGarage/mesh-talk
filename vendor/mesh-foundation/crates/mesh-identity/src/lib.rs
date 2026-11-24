//! Mesh Talk identity and device certificate formats.
//! Applications establish trust and decide how to enroll and revoke devices.

//! # Example
//!
//! ```
//! use mesh_identity::device::DeviceIdentity;
//!
//! let device = DeviceIdentity::generate();
//! let message = b"hello mesh";
//! let signature = device.sign(message);
//! assert!(DeviceIdentity::verify(&device.public().ed25519_pub, message, &signature));
//! ```
//!
//! Signature verification does not establish account trust or authorize access.

pub mod account;
pub mod device;

use ed25519_dalek::SigningKey;
use rand_core::RngCore;

fn random_signing_key() -> SigningKey {
    let mut secret = zeroize::Zeroizing::new([0u8; 32]);
    rand_core::OsRng.fill_bytes(&mut *secret);
    SigningKey::from_bytes(&secret)
}
