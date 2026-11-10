//! Release-only verifier/roundtrip helper. Never installs or launches an artifact.
#[path = "../src/updates/delta_codec.rs"]
mod delta_codec;

use base64::Engine;
use std::{fs::File, io::Read, path::Path};

#[derive(Debug)]
enum VerificationError {
    MissingVersion,
    Invalid,
}

fn verify(
    bytes: &[u8],
    signature: &str,
    public_key: &str,
    version: &str,
) -> Result<(), VerificationError> {
    use minisign_verify::{PublicKey, Signature};
    let decode = |text: &str| -> Result<String, VerificationError> {
        if text.len() > 16 * 1024 {
            return Err(VerificationError::Invalid);
        }
        let raw = base64::engine::general_purpose::STANDARD
            .decode(text.trim())
            .map_err(|_| VerificationError::Invalid)?;
        String::from_utf8(raw).map_err(|_| VerificationError::Invalid)
    };
    let key = PublicKey::decode(&decode(public_key)?).map_err(|_| VerificationError::Invalid)?;
    let signature =
        Signature::decode(&decode(signature)?).map_err(|_| VerificationError::Invalid)?;
    // This is the SDK 2.14 verifier's exact cryptographic operation, including
    // the global signature over the trusted comment. Never read its version first.
    key.verify(bytes, &signature, true)
        .map_err(|_| VerificationError::Invalid)?;
    let mut versions = signature
        .trusted_comment()
        .split('\t')
        .filter_map(|field| field.strip_prefix("version:"));
    let signed = versions.next().ok_or(VerificationError::MissingVersion)?;
    // Publisher inputs are canonical logical identities. Be stricter than the
    // SDK's accepted optional v-prefix spelling; duplicate fields are rejected.
    if versions.next().is_some() || signed != version {
        return Err(VerificationError::Invalid);
    }
    Ok(())
}

fn read_bounded(path: &Path, maximum: u64) -> Result<Vec<u8>, String> {
    let file = File::open(path).map_err(|_| "Could not read the release input")?;
    if file
        .metadata()
        .map_err(|_| "Could not inspect the release input")?
        .len()
        > maximum
    {
        return Err("Release input exceeds its size limit".into());
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the release input")?;
    if bytes.len() as u64 > maximum {
        return Err("Release input exceeds its size limit".into());
    }
    Ok(bytes)
}

fn run(args: &[String]) -> Result<(), (i32, String)> {
    let invalid = |message: &str| (1, message.to_owned());
    match args.first().map(String::as_str) {
        Some("verify") if args.len() == 5 => {
            let bytes =
                read_bounded(Path::new(&args[1]), 1024 * 1024 * 1024).map_err(|e| (1, e))?;
            let text = |arg: &str| {
                read_bounded(Path::new(arg), 16 * 1024).and_then(|bytes| {
                    String::from_utf8(bytes).map_err(|_| "Invalid trust metadata".into())
                })
            };
            let signature = text(&args[2]).map_err(|e| (1, e))?;
            let key = text(&args[3]).map_err(|e| (1, e))?;
            verify(&bytes, &signature, &key, &args[4]).map_err(|error| match error {
                VerificationError::MissingVersion => (
                    3,
                    "Authentic legacy signature has no signed logical version".into(),
                ),
                VerificationError::Invalid => {
                    invalid("Release signature or full logical version is invalid")
                }
            })
        }
        Some("reconstruct") if args.len() == 6 => {
            let base = read_bounded(Path::new(&args[1]), delta_codec::MAX_BASE_SIZE as u64)
                .map_err(|e| (1, e))?;
            let patch = read_bounded(Path::new(&args[2]), delta_codec::MAX_OUTPUT_SIZE)
                .map_err(|e| (1, e))?;
            let size = args[4]
                .parse::<u64>()
                .map_err(|_| invalid("Invalid target size"))?;
            let mut output = File::create_new(&args[3])
                .map_err(|_| invalid("Output must be a new private staging file"))?;
            let result =
                delta_codec::reconstruct(&base, &patch, &mut output, size, &args[5], || false)
                    .and_then(|()| output.sync_all().map_err(delta_codec::DeltaError::Io));
            drop(output);
            if let Err(error) = result {
                let _ = std::fs::remove_file(&args[3]);
                return Err((1, error.to_string()));
            }
            Ok(())
        }
        _ => Err(invalid(
            "Expected verify FILE SIGFILE PUBLICKEYFILE FULL_VERSION or reconstruct BASE PATCH OUTPUT SIZE SHA256",
        )),
    }
}

fn main() {
    if let Err((code, message)) = run(&std::env::args().skip(1).collect::<Vec<_>>()) {
        eprintln!("{message}");
        std::process::exit(code);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    #[test]
    fn authenticates_bytes_and_full_signed_version_with_the_existing_root() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../src/updates/fixtures/delta-native.json"))
                .unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(fixture["target"].as_str().unwrap())
            .unwrap();
        let signature = fixture["target_signature"].as_str().unwrap();
        let key = fixture["public_key"].as_str().unwrap();
        assert!(matches!(
            verify(
                &bytes,
                fixture["legacy_signature"].as_str().unwrap(),
                key,
                "99.0.0-nightly.42.1"
            ),
            Err(VerificationError::MissingVersion)
        ));

        assert!(verify(&bytes, signature, key, "99.0.0-nightly.42.1").is_ok());
        assert!(verify(&bytes, signature, key, "99.0.0-nightly.42.2").is_err());
        assert!(verify(b"modified", signature, key, "99.0.0-nightly.42.1").is_err());
        assert!(verify(&bytes, signature, "invalid", "99.0.0-nightly.42.1").is_err());
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(signature)
            .unwrap();
        let forged = String::from_utf8(decoded)
            .unwrap()
            .replace("version:99.0.0-nightly.42.1", "version:99.0.0-nightly.42.2");
        let forged = base64::engine::general_purpose::STANDARD.encode(forged);
        assert!(verify(&bytes, &forged, key, "99.0.0-nightly.42.2").is_err());
    }
}
