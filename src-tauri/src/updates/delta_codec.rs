//! Bounded reconstruction of the exact native installer from a zstd prefix frame.

use std::{
    io::{self, Write},
    time::{Duration, Instant},
};

pub(crate) const MAX_BASE_SIZE: usize = 128 * 1024 * 1024;
pub(crate) const MAX_OUTPUT_SIZE: u64 = 256 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum DeltaError {
    Canceled,
    Deadline,
    Invalid(String),
    Io(io::Error),
}

impl std::fmt::Display for DeltaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Canceled => f.write_str("Update canceled"),
            Self::Deadline => f.write_str("Delta reconstruction exceeded its deadline"),
            Self::Invalid(message) => f.write_str(message),
            Self::Io(error) => write!(f, "Could not write reconstructed installer: {error}"),
        }
    }
}

impl std::error::Error for DeltaError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

/// Run on a blocking worker with a caller-owned private output file. On any error,
/// discard the output: it may contain incomplete or unverified installer bytes.
pub(crate) fn reconstruct(
    base: &[u8],
    patch: &[u8],
    output: impl Write,
    expected_size: u64,
    expected_sha256: &str,
    canceled: impl Fn() -> bool,
) -> Result<(), DeltaError> {
    let start = Instant::now();
    reconstruct_with_clock(
        base,
        patch,
        output,
        expected_size,
        expected_sha256,
        canceled,
        || start.elapsed(),
    )
}

fn reconstruct_with_clock(
    base: &[u8],
    patch: &[u8],
    mut output: impl Write,
    expected_size: u64,
    expected_sha256: &str,
    canceled: impl Fn() -> bool,
    elapsed: impl Fn() -> Duration,
) -> Result<(), DeltaError> {
    use sha2::{Digest, Sha256};
    use zstd::stream::raw::{DParameter, Decoder, Operation};

    let checkpoint = || {
        if canceled() {
            return Err(DeltaError::Canceled);
        }
        if elapsed() >= Duration::from_secs(60) {
            return Err(DeltaError::Deadline);
        }
        Ok(())
    };
    checkpoint()?;
    if base.len() > MAX_BASE_SIZE || expected_size > MAX_OUTPUT_SIZE {
        return Err(DeltaError::Invalid(
            "Delta reconstruction size exceeds its limit".into(),
        ));
    }
    if expected_sha256.len() != 64
        || !expected_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(DeltaError::Invalid("Invalid target SHA-256".into()));
    }
    validate_frame_header(patch, expected_size)?;
    let invalid_frame =
        |error: io::Error| DeltaError::Invalid(format!("Invalid zstd prefix frame: {error}"));
    let mut decoder = Decoder::with_ref_prefix(base).map_err(invalid_frame)?;
    decoder
        .set_parameter(DParameter::WindowLogMax(27))
        .map_err(invalid_frame)?;
    let mut buffer = [0u8; 64 * 1024];
    let mut position = 0usize;
    let mut written = 0u64;
    let mut digest = Sha256::new();
    loop {
        checkpoint()?;
        // Bound both compressed work and output before checking cancellation/deadline again.
        let end = patch.len().min(position.saturating_add(buffer.len()));
        let status = decoder
            .run_on_buffers(&patch[position..end], &mut buffer)
            .map_err(invalid_frame)?;
        position += status.bytes_read;
        checkpoint()?;
        written += status.bytes_written as u64;
        if written > expected_size {
            return Err(DeltaError::Invalid(
                "Reconstructed installer exceeds its expected size".into(),
            ));
        }
        let chunk = &buffer[..status.bytes_written];
        output.write_all(chunk).map_err(DeltaError::Io)?;
        digest.update(chunk);
        if status.remaining == 0 {
            if position != patch.len() {
                return Err(DeltaError::Invalid(
                    "Trailing data after zstd prefix frame".into(),
                ));
            }
            break;
        }
        if status.bytes_read == 0 && status.bytes_written == 0 {
            return Err(DeltaError::Invalid("Truncated zstd prefix frame".into()));
        }
    }
    checkpoint()?;
    if written != expected_size {
        return Err(DeltaError::Invalid(
            "Reconstructed installer size does not match".into(),
        ));
    }
    if hex::encode(digest.finalize()) != expected_sha256 {
        return Err(DeltaError::Invalid(
            "Reconstructed installer SHA-256 does not match".into(),
        ));
    }
    Ok(())
}

// zstd's decoder may bypass WindowLogMax when a known-size frame fits its output
// buffer. Enforce the declared window before creating the decoder as well.
fn validate_frame_header(patch: &[u8], expected_size: u64) -> Result<(), DeltaError> {
    let invalid = || DeltaError::Invalid("Invalid zstd prefix frame header".into());
    // Only ordinary frames are patch payloads; skippable frames are excluded.
    if !patch.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        return Err(invalid());
    }
    let descriptor = *patch.get(4).ok_or_else(invalid)?;
    if descriptor & 0x08 != 0 {
        return Err(invalid()); // RFC 8878 section 3.1.1.1.1.4: reserved bit.
    }
    // The stable safe API validates the complete variable-length frame header,
    // including dictionary-ID and content-size field lengths.
    let content_size = zstd::zstd_safe::get_frame_content_size(patch).map_err(|_| invalid())?;
    let window_size = if descriptor & 0x20 != 0 {
        content_size.ok_or_else(invalid)?
    } else {
        // RFC 8878 section 3.1.1.1.2: windowBase + windowBase/8 * mantissa.
        let window = *patch.get(5).ok_or_else(invalid)?;
        let base = 1u64 << (10 + (window >> 3));
        base + (base / 8) * u64::from(window & 7)
    };
    if window_size > 128 * 1024 * 1024 {
        return Err(DeltaError::Invalid(
            "Zstd prefix frame window exceeds its limit".into(),
        ));
    }
    if content_size.is_some_and(|size| size != expected_size) {
        return Err(DeltaError::Invalid(
            "Zstd prefix frame content size does not match".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::cell::Cell;

    fn digest(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn fixture() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let base: Vec<_> = (0..128 * 1024)
            .map(|n| ((n * 17 + n / 251) % 256) as u8)
            .collect();
        let mut target = base.clone();
        target.extend_from_slice(b"new installer tail");
        let mut encoder =
            zstd::stream::write::Encoder::with_ref_prefix(Vec::new(), 3, &base).unwrap();
        encoder.write_all(&target).unwrap();
        let patch = encoder.finish().unwrap();
        (base, target, patch)
    }

    #[test]
    fn reconstructs_exact_installer_bytes() {
        let (base, target, patch) = fixture();
        assert!(patch.len() < target.len() / 10);
        assert!(
            zstd::stream::decode_all(patch.as_slice()).is_err(),
            "fixture must require the base prefix"
        );
        let mut output = Vec::new();
        reconstruct(
            &base,
            &patch,
            &mut output,
            target.len() as u64,
            &digest(&target),
            || false,
        )
        .unwrap();
        assert_eq!(output, target);
    }

    #[test]
    fn reconstructs_across_compressed_input_chunks() {
        let mut state = 71u64;
        let target: Vec<_> = (0..200_000)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let patch = zstd::stream::encode_all(target.as_slice(), 1).unwrap();
        assert!(patch.len() > 64 * 1024);
        let mut output = Vec::new();
        reconstruct(
            b"",
            &patch,
            &mut output,
            target.len() as u64,
            &digest(&target),
            || false,
        )
        .unwrap();
        assert_eq!(output, target);
    }

    #[test]
    fn reports_output_failure_without_claiming_success() {
        struct BrokenOutput;
        impl Write for BrokenOutput {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disk full"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (base, target, patch) = fixture();
        let error = reconstruct(
            &base,
            &patch,
            BrokenOutput,
            target.len() as u64,
            &digest(&target),
            || false,
        )
        .unwrap_err();
        assert!(matches!(&error, DeltaError::Io(_)));
        assert!(error.to_string().contains("disk full"));
    }

    #[test]
    fn rejects_base_over_the_memory_budget() {
        let error = reconstruct(
            &vec![0; MAX_BASE_SIZE + 1],
            b"",
            io::sink(),
            0,
            &digest(b""),
            || false,
        )
        .unwrap_err();
        assert!(matches!(error, DeltaError::Invalid(message) if message.contains("size exceeds")));
    }

    #[test]
    fn rejects_wrong_base_and_digest() {
        let (base, target, patch) = fixture();
        assert!(
            reconstruct(
                &vec![42; base.len()],
                &patch,
                io::sink(),
                target.len() as u64,
                &digest(&target),
                || false
            )
            .is_err()
        );
        assert!(
            reconstruct(
                &base,
                &patch,
                io::sink(),
                target.len() as u64,
                &"0".repeat(64),
                || false
            )
            .is_err()
        );
    }

    #[test]
    fn rejects_truncated_and_trailing_data_or_frames() {
        let (base, target, patch) = fixture();
        for candidate in [
            patch[..patch.len() - 1].to_vec(),
            [patch.as_slice(), b"garbage"].concat(),
            [patch.as_slice(), patch.as_slice()].concat(),
        ] {
            assert!(
                reconstruct(
                    &base,
                    &candidate,
                    io::sink(),
                    target.len() as u64,
                    &digest(&target),
                    || false
                )
                .is_err()
            );
        }
    }

    #[test]
    fn never_writes_beyond_expected_size() {
        let (base, target, patch) = fixture();
        let mut output = Vec::new();
        assert!(reconstruct(&base, &patch, &mut output, 17, &digest(&target), || false).is_err());
        assert!(output.len() <= 17);
    }

    #[test]
    fn checks_cancellation_between_chunks() {
        let (base, target, patch) = fixture();
        let calls = Cell::new(0);
        let mut output = Vec::new();
        let error = reconstruct(
            &base,
            &patch,
            &mut output,
            target.len() as u64,
            &digest(&target),
            || {
                calls.set(calls.get() + 1);
                calls.get() >= 3
            },
        )
        .unwrap_err();
        assert!(matches!(error, DeltaError::Canceled));
        assert!(output.len() < target.len());
    }

    #[test]
    fn enforces_sixty_second_deadline_between_chunks() {
        let (base, target, patch) = fixture();
        let calls = Cell::new(0);
        let error = reconstruct_with_clock(
            &base,
            &patch,
            io::sink(),
            target.len() as u64,
            &digest(&target),
            || false,
            || {
                calls.set(calls.get() + 1);
                Duration::from_secs(if calls.get() >= 3 { 60 } else { 0 })
            },
        )
        .unwrap_err();
        assert!(matches!(error, DeltaError::Deadline));
    }

    #[test]
    fn accepts_window_boundary_and_rejects_next_mantissa_step() {
        for (window, accepted) in [(0x87, true), (0x88, true), (0x89, false), (0xff, false)] {
            let header = [0x28, 0xb5, 0x2f, 0xfd, 0, window];
            assert_eq!(validate_frame_header(&header, 0).is_ok(), accepted);
        }
        for size in [128u32 * 1024 * 1024, 128 * 1024 * 1024 + 1] {
            let mut header = vec![0x28, 0xb5, 0x2f, 0xfd, 0xa0];
            header.extend_from_slice(&size.to_le_bytes());
            assert_eq!(
                validate_frame_header(&header, u64::from(size)).is_ok(),
                size == 128 * 1024 * 1024
            );
        }
    }

    #[test]
    fn rejects_truncated_variable_headers_reserved_bit_and_skippable_frame() {
        // Single-segment frame, four-byte dictionary ID, eight-byte content size.
        let mut header = vec![0x28, 0xb5, 0x2f, 0xfd, 0xe3, 0, 0, 0, 0];
        header.extend_from_slice(&256u64.to_le_bytes());
        assert!(validate_frame_header(&header, 256).is_ok());
        for length in 0..header.len() {
            assert!(validate_frame_header(&header[..length], 256).is_err());
        }
        header[4] |= 0x08;
        assert!(validate_frame_header(&header, 256).is_err());
        assert!(validate_frame_header(&[0x50, 0x2a, 0x4d, 0x18, 0, 0, 0, 0], 0).is_err());
    }

    #[test]
    fn validates_declared_content_size_before_decoding() {
        // A two-byte content-size field encodes 256 + its little-endian value.
        let header = [0x28, 0xb5, 0x2f, 0xfd, 0x40, 0x88, 1, 0];
        assert!(validate_frame_header(&header, 257).is_ok());
        assert!(validate_frame_header(&header, 256).is_err());
    }

    #[test]
    fn rejects_declared_large_window_even_for_small_known_output() {
        let target = vec![0; 256];
        let mut patch = vec![0x28, 0xb5, 0x2f, 0xfd, 0x40, 0x90, 0, 0, 1, 8, 0];
        patch.extend_from_slice(&target);
        assert_eq!(zstd::stream::decode_all(patch.as_slice()).unwrap(), target);
        let mut output = Vec::new();
        assert!(reconstruct(b"", &patch, &mut output, 256, &digest(&target), || false).is_err());
        assert!(output.is_empty());
    }

    #[test]
    fn rejects_excessive_window_before_allocating_it() {
        // Ordinary frame, no content size, window descriptor 0x90 = 256 MiB.
        let patch = [0x28, 0xb5, 0x2f, 0xfd, 0, 0x90, 1, 0, 0];
        assert!(reconstruct(b"", &patch, io::sink(), 0, &digest(b""), || false).is_err());
    }

    #[test]
    fn rejects_oversized_metadata_and_noncanonical_digest() {
        for (size, hash) in [
            (256 * 1024 * 1024 + 1, digest(b"")),
            (0, "A".repeat(64)),
            (0, "invalid".into()),
        ] {
            assert!(reconstruct(b"", b"", io::sink(), size, &hash, || false).is_err());
        }
    }
}
