//! A Noise transport-mode session: encrypts/decrypts individual messages
//! once the handshake has completed. One [`Session`] per peer connection.

use crate::transport::{TransportError, MAX_PLAINTEXT};

/// Encrypts and decrypts messages over an established Noise channel.
pub struct Session {
    transport: snow::TransportState,
}

impl Session {
    /// Wrap a completed Noise transport state.
    pub(crate) fn new(transport: snow::TransportState) -> Self {
        Self { transport }
    }

    /// Encrypt one message, returning the Noise ciphertext blob (<= `MAX_FRAME`).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>, TransportError> {
        if plaintext.len() > MAX_PLAINTEXT {
            return Err(TransportError::PlaintextTooLarge(plaintext.len()));
        }
        // ChaChaPoly adds a 16-byte tag; size the buffer exactly.
        let mut buf = vec![0u8; plaintext.len() + 16];
        let len = self.transport.write_message(plaintext, &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }

    /// Decrypt one Noise ciphertext blob back into plaintext.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>, TransportError> {
        // Plaintext is never larger than the ciphertext; size the buffer to it.
        let mut buf = vec![0u8; ciphertext.len()];
        let len = self.transport.read_message(ciphertext, &mut buf)?;
        buf.truncate(len);
        Ok(buf)
    }
}
