//! Compatibility exports for account identities and device certificates.

#[cfg(test)]
pub(crate) use mesh_identity::account::certificate_observations;
pub use mesh_identity::account::{
    Account, AccountPublic, DeviceCertificate, MAX_ACCOUNT_CREATED_AT_MS,
};
