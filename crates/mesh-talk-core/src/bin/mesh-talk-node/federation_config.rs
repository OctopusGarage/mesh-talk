//! Versioned public trust pins shared by equal relays; private identities remain local.
use clap::{Parser, Subcommand};
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

const HEADER: &str = "mesh-talk-federation-v1";
const MAX_BYTES: u64 = 65_536;
const MAX_SEEDS: usize = 256;

#[derive(Clone, Debug)]
pub(super) struct FederationSeed {
    pub(super) id: String,
    pub(super) discovery: SocketAddr,
}

impl std::str::FromStr for FederationSeed {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (id, endpoint) = value
            .split_once('@')
            .ok_or_else(|| "expected <full-fingerprint>@<ip>:<discovery-port>".to_string())?;
        let id = parse_fingerprint(id)?;
        let discovery = endpoint
            .parse::<SocketAddr>()
            .map_err(|_| "expected a numeric routed IP and UDP port".to_string())?;
        if discovery.port() == 0 {
            return Err("federation discovery port must be nonzero".into());
        }
        Ok(Self { id, discovery })
    }
}

pub(super) fn parse_fingerprint(value: &str) -> Result<String, String> {
    if value.len() == 32 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(value.to_ascii_lowercase())
    } else {
        Err("expected a full 32-character hexadecimal device fingerprint".into())
    }
}

#[derive(Parser)]
#[command(
    name = "mesh-talk-node federation",
    about = "Manage public relay trust pins; restart every affected relay after changes"
)]
struct Cli {
    #[arg(long)]
    file: PathBuf,
    #[command(subcommand)]
    operation: Operation,
}

#[derive(Subcommand)]
enum Operation {
    /// Create an empty versioned manifest, refusing to overwrite an existing file.
    Init,
    /// List normalized public pins without loading any private identity.
    List,
    /// Pin an explicitly approved identity and numeric discovery endpoint.
    Enroll { seed: FederationSeed },
    /// Remove every endpoint for a full identity. Running relays must be restarted.
    Revoke {
        #[arg(value_parser = parse_fingerprint)]
        full_id: String,
    },
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

pub(super) fn load(path: &Path) -> io::Result<Vec<FederationSeed>> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid("federation manifest exceeds 65536 bytes"));
    }
    let text =
        std::str::from_utf8(&bytes).map_err(|_| invalid("federation manifest must be UTF-8"))?;
    let mut lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    if lines.next() != Some(HEADER) {
        return Err(invalid(
            "unsupported federation manifest; expected mesh-talk-federation-v1",
        ));
    }
    let mut entries = Vec::new();
    for line in lines {
        if entries.len() == MAX_SEEDS {
            return Err(invalid("federation manifest exceeds 256 entries"));
        }
        entries.push(line.parse::<FederationSeed>().map_err(invalid)?);
    }
    normalize(&mut entries);
    Ok(entries)
}

fn normalize(entries: &mut Vec<FederationSeed>) {
    let mut seen = BTreeSet::new();
    entries.retain(|seed| seen.insert((seed.id.clone(), seed.discovery)));
    entries.sort_by(|a, b| (&a.id, a.discovery).cmp(&(&b.id, b.discovery)));
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

fn save(path: &Path, entries: &[FederationSeed], create: bool) -> io::Result<()> {
    let mut contents = format!("{HEADER}\n");
    for seed in entries {
        use std::fmt::Write as _;
        writeln!(contents, "{}@{}", seed.id, seed.discovery).expect("writing to a string");
    }
    if entries.len() > MAX_SEEDS || contents.len() as u64 > MAX_BYTES {
        return Err(invalid(
            "federation manifest exceeds bounded size or entry count",
        ));
    }
    // Write and sync beside the destination, then publish with one atomic rename.
    let mut temporary = tempfile::NamedTempFile::new_in(parent(path))?;
    temporary.write_all(contents.as_bytes())?;
    temporary.as_file().sync_all()?;
    if create {
        temporary.persist_noclobber(path).map_err(|err| err.error)?;
    } else {
        temporary.persist(path).map_err(|err| err.error)?;
    }
    // Directory syncing makes the rename durable on Unix filesystems.
    #[cfg(unix)]
    File::open(parent(path))?.sync_all()?;
    Ok(())
}

/// Hold a stable companion inode across load, replacement, and directory sync.
/// Never unlink it: a second inode would let concurrent writers bypass the lock.
fn lock_mutation(path: &Path) -> io::Result<(PathBuf, File)> {
    let filename = path
        .file_name()
        .ok_or_else(|| invalid("expected a manifest filename"))?;
    let directory = parent(path).canonicalize()?;
    let mut lock_filename = filename.to_os_string();
    lock_filename.push(".lock");
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(directory.join(lock_filename))?;
    lock.try_lock().map_err(|error| {
        io::Error::other(format!(
            "federation manifest mutation lock unavailable; retry after the other update finishes: {error}"
        ))
    })?;
    Ok((directory.join(filename), lock))
}

pub(super) fn run_cli() -> Result<(), Box<dyn std::error::Error>> {
    // Keep the existing node argument contract while offering a keystore-free command.
    let cli = Cli::parse_from(std::env::args_os().skip(1));
    if matches!(cli.operation, Operation::List) {
        for seed in load(&cli.file)? {
            println!("{}@{}", seed.id, seed.discovery);
        }
        return Ok(());
    }
    // List and startup loading stay lock-free; every read/modify/write command
    // takes this exclusive, nonblocking OS lock before reading its snapshot.
    let (path, _mutation_lock) = lock_mutation(&cli.file)?;
    match cli.operation {
        Operation::Init => save(&path, &[], true)?,
        Operation::List => unreachable!("handled before acquiring mutation lock"),
        Operation::Enroll { seed } => {
            let mut entries = load(&path)?;
            entries.push(seed);
            normalize(&mut entries);
            save(&path, &entries, false)?;
        }
        Operation::Revoke { full_id } => {
            let mut entries = load(&path)?;
            entries.retain(|seed| seed.id != full_id);
            save(&path, &entries, false)?;
        }
    }
    eprintln!(
        "Public federation trust file updated; restart required on every affected relay. Running relays retain their startup trust snapshot."
    );
    Ok(())
}
