pub mod account;
pub mod account_keystore;
pub mod auth;
pub mod device;
pub mod errors;
pub mod keys;
pub mod keystore;
pub mod manager;
pub mod user;

use ed25519_dalek::SigningKey;
use rand_core::RngCore;

pub(crate) fn random_signing_key() -> SigningKey {
    let mut secret = [0u8; 32];
    rand_core::OsRng.fill_bytes(&mut secret);
    SigningKey::from_bytes(&secret)
}

#[cfg(test)]
mod tests;
