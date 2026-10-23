//! Authenticated deadline framing for expiring message and file plaintext.

const MAGIC: &[u8; 5] = b"MTEX1";
const HEADER: usize = MAGIC.len() + 8;

/// Wrap a plaintext payload with a fixed UTC deadline. `None` preserves the legacy wire.
pub(crate) fn wrap(body: &[u8], expires_at_ms: Option<u64>) -> Vec<u8> {
    let Some(deadline) = expires_at_ms else {
        return body.to_vec();
    };
    // Keep the zero sentinel outermost: deployed body decoders only split once.
    if let Some(protected) = body.strip_prefix(b"MTEX1\0\0\0\0\0\0\0\0") {
        let mut wire = b"MTEX1\0\0\0\0\0\0\0\0".to_vec();
        wire.extend_from_slice(MAGIC);
        wire.extend_from_slice(&deadline.to_le_bytes());
        wire.extend_from_slice(protected);
        return wire;
    }
    let mut wire = Vec::with_capacity(HEADER + body.len());
    wire.extend_from_slice(MAGIC);
    wire.extend_from_slice(&deadline.to_le_bytes());
    wire.extend_from_slice(body);
    wire
}

/// Return the deadline and original bytes. A truncated deadline frame fails closed.
pub(crate) fn split(wire: &[u8]) -> Option<(Option<u64>, &[u8])> {
    let Some(rest) = wire.strip_prefix(MAGIC) else {
        return Some((None, wire));
    };
    let (time, body) = rest.split_at_checked(8)?;
    let deadline = u64::from_le_bytes(time.try_into().ok()?);
    if deadline == 0 {
        let (deadline, content) = if let Some(rest) = body.strip_prefix(MAGIC) {
            let (time, content) = rest.split_at_checked(8)?;
            let deadline = u64::from_le_bytes(time.try_into().ok()?);
            if deadline == 0 {
                return None;
            }
            (Some(deadline), content)
        } else {
            (None, body)
        };
        return content.starts_with(b"MTPR1").then_some((deadline, content));
    }
    if body.is_empty() {
        return None;
    }
    Some((Some(deadline), body))
}

/// A valid deadline in the authenticated plaintext, if present.
pub(crate) fn deadline(wire: &[u8]) -> Option<u64> {
    split(wire)?.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_deadline_round_trips_and_legacy_payload_stays_unchanged() {
        let body = b"hello";
        assert_eq!(split(body), Some((None, body.as_slice())));
        let wire = wrap(body, Some(1_700_000_000_000));
        assert_eq!(
            split(&wire),
            Some((Some(1_700_000_000_000), body.as_slice()))
        );
        assert_eq!(wrap(body, None), body);
    }

    #[test]
    fn malformed_deadline_frame_is_rejected() {
        assert_eq!(split(b"MTEX1"), None);
        assert_eq!(split(b"MTEX1\x00\x00"), None);
    }
}
