//! Offline relay-network administration. Only the enrollment workstation holds
//! the private authority key; relays receive public policy and device approval.
use super::federation_config::{lock_mutation, parse_fingerprint};
use clap::{Parser, Subcommand};
use mesh_talk_core::identity::device::DeviceIdentity;
use mesh_talk_core::postoffice::network::{NetworkMembership, NetworkPolicy, unix_now};
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

#[derive(Parser)]
#[command(
    name = "mesh-talk-node network",
    about = "Manage operator-approved post-office networks; membership does not attest software"
)]
struct Cli {
    #[command(subcommand)]
    operation: Operation,
}

#[derive(Subcommand)]
enum Operation {
    /// Create an offline authority key and public network policy. Never overwrites files.
    Init {
        #[arg(long)]
        authority_key: PathBuf,
        #[arg(long)]
        policy: PathBuf,
    },
    /// Issue a nondelegating certificate for a verified relay device fingerprint.
    Enroll {
        #[arg(long)]
        authority_key: PathBuf,
        #[arg(long)]
        policy: PathBuf,
        #[arg(long, value_parser = parse_fingerprint)]
        device: String,
        #[arg(long)]
        membership: PathBuf,
        #[arg(long, default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..=90))]
        valid_days: u64,
    },
    /// Inspect public trust, optional membership validity, and attestation limits.
    Inspect {
        #[arg(long)]
        policy: PathBuf,
        #[arg(long)]
        membership: Option<PathBuf>,
    },
    /// Revoke an entire device. Distribute policy and restart every affected relay.
    Revoke {
        #[arg(long)]
        policy: PathBuf,
        #[arg(long, value_parser = parse_fingerprint)]
        device: String,
    },
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn bounded_read(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid("network file exceeds size limit"));
    }
    Ok(bytes)
}

pub(super) fn load_policy(path: &Path) -> io::Result<NetworkPolicy> {
    let bytes = bounded_read(path, 65_536).map_err(|e| invalid(format!("network policy: {e}")))?;
    NetworkPolicy::decode(&bytes).map_err(invalid)
}

pub(super) fn load_membership(path: &Path) -> io::Result<NetworkMembership> {
    NetworkMembership::decode(&bounded_read(path, 4096)?).map_err(invalid)
}

fn save(path: &Path, bytes: &[u8], create: bool) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    if create {
        temporary.persist_noclobber(path).map_err(|e| e.error)?;
    } else {
        temporary.persist(path).map_err(|e| e.error)?;
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

fn load_authority(path: &Path) -> io::Result<DeviceIdentity> {
    if std::fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(invalid("authority key must be a private regular file"));
    }
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() != 32 {
        return Err(invalid(
            "authority key must be a 32-byte private regular file",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(invalid(
                "authority key permissions must be 0600 or stricter",
            ));
        }
    }
    let mut bytes = Zeroizing::new([0u8; 32]);
    file.read_exact(bytes.as_mut())?;
    Ok(DeviceIdentity::from_secret_bytes(*bytes, [0; 32]))
}

pub(super) fn run_cli() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse_from(std::env::args_os().skip(1));
    match cli.operation {
        Operation::Init {
            authority_key,
            policy,
        } => {
            let (policy, _policy_lock) = lock_mutation(&policy)?;
            let (authority_key, _key_lock) = lock_mutation(&authority_key)?;
            if policy.try_exists()? || authority_key.try_exists()? {
                return Err(invalid(
                    "network init refuses to overwrite an authority key or policy",
                )
                .into());
            }
            let authority = DeviceIdentity::generate();
            let secret = Zeroizing::new(authority.secret_bytes().0);
            let public = NetworkPolicy::new(authority.public().ed25519_pub)
                .encode()
                .map_err(invalid)?;
            save(&authority_key, secret.as_ref(), true)?;
            save(&policy, &public, true)?;
            println!(
                "Network created. Keep the authority key offline; distribute only the public policy and device memberships."
            );
        }
        Operation::Enroll {
            authority_key,
            policy,
            device,
            membership,
            valid_days,
        } => {
            let (policy, _policy_lock) = lock_mutation(&policy)?;
            let policy = load_policy(&policy)?;
            let authority = load_authority(&authority_key)?;
            if authority.public().ed25519_pub != policy.authority() {
                return Err(invalid("authority key does not match network policy").into());
            }
            let certificate = NetworkMembership::issue(
                &authority,
                &device,
                unix_now(),
                Duration::from_secs(valid_days * 86400),
            )
            .map_err(invalid)?;
            certificate
                .verify_approval(&policy, unix_now())
                .map_err(invalid)?;
            let (path, _membership_lock) = lock_mutation(&membership)?;
            save(&path, &certificate.encode().map_err(invalid)?, true)?;
            println!(
                "Membership issued for {device}; expires at {} (Unix seconds). Install on that relay and restart it.",
                certificate.expires_at()
            );
        }
        Operation::Inspect { policy, membership } => {
            let policy = load_policy(&policy)?;
            println!("Network authority: {}", hex::encode(policy.authority()));
            for device in policy.revoked() {
                println!("Revoked device: {device}");
            }
            if let Some(path) = membership {
                let membership = load_membership(&path)?;
                membership
                    .verify_approval(&policy, unix_now())
                    .map_err(invalid)?;
                println!(
                    "Approved device: {}; expires at {} (Unix seconds)",
                    membership.device_id(),
                    membership.expires_at()
                );
            }
            println!(
                "Software integrity: not attested. Membership does not establish an official build or a malware-free host."
            );
            println!("Policy changes require distribution and restart on every affected relay.");
        }
        Operation::Revoke { policy, device } => {
            let (path, _policy_lock) = lock_mutation(&policy)?;
            let mut policy = load_policy(&path)?;
            policy.revoke(&device).map_err(invalid)?;
            save(&path, &policy.encode().map_err(invalid)?, false)?;
            println!(
                "Revoked {device}. Distribute the public policy and restart every affected relay; running relays retain their startup snapshot."
            );
        }
    }
    Ok(())
}
