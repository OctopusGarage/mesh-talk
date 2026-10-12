//! Per-export recovery key. Never persist or log this value.

use rand_core::{OsRng, RngCore};
use zeroize::Zeroize;

pub struct RecoveryKey([u8; 32]);

impl RecoveryKey {
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    pub fn parse(input: &str) -> Result<Self, InvalidRecoveryKey> {
        let digits: String = input
            .chars()
            .filter(|c| !c.is_ascii_whitespace() && *c != '-')
            .collect();
        if digits.len() != 64 {
            return Err(InvalidRecoveryKey);
        }
        let mut bytes = [0u8; 32];
        hex::decode_to_slice(&digits, &mut bytes).map_err(|_| InvalidRecoveryKey)?;
        Ok(Self(bytes))
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn display(&self) -> String {
        let digits = hex::encode(self.0);
        digits
            .as_bytes()
            .chunks(8)
            .map(|chunk| std::str::from_utf8(chunk).expect("hex is ASCII"))
            .collect::<Vec<_>>()
            .join("-")
    }
}

impl Drop for RecoveryKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryKey([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidRecoveryKey;

impl std::fmt::Display for InvalidRecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid recovery key")
    }
}

impl std::error::Error for InvalidRecoveryKey {}
