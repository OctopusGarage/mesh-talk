//! Participant control metadata only. All history and crypto domains remain
//! legacy sources until their coordinated publication barrier is implemented.
use super::{
    ProfileStorageLease,
    manifest::{self, invalid},
    publication_files,
};
use crate::{
    eventlog::LogError,
    identity::{account::AccountPublic, device::PublicIdentity},
    storage::database::EncryptedDatabase,
};
use rusqlite::params;
use std::path::{Path, PathBuf};

const SCHEMA: &str = "
CREATE TABLE participant_profile(singleton INTEGER PRIMARY KEY CHECK(singleton=1), version INTEGER NOT NULL CHECK(version=1), role TEXT NOT NULL CHECK(role='participant-v1'), account BLOB NOT NULL CHECK(length(account)=32), next_account BLOB CHECK(next_account IS NULL OR length(next_account)=32), phase INTEGER NOT NULL CHECK(phase IN (0,1)), bootstrap_previous TEXT CHECK(bootstrap_previous IS NULL OR length(bootstrap_previous)=32), CHECK((phase=0 AND next_account IS NULL) OR (phase=1 AND next_account IS NOT NULL AND next_account<>account)));
CREATE TABLE participant_sources(domain INTEGER PRIMARY KEY CHECK(domain IN (0,1)), locator BLOB NOT NULL UNIQUE);
CREATE TABLE participant_account_namespaces(account_id TEXT PRIMARY KEY CHECK(length(account_id)=32), account BLOB UNIQUE CHECK(account IS NULL OR length(account)=32));
";

pub(crate) struct ParticipantOwner {
    pub identity: PublicIdentity,
    pub account: AccountPublic,
    pub paths: ParticipantPaths,
}

pub(crate) struct ParticipantPaths {
    pub events: PathBuf,
    pub sent: PathBuf,
}

pub(super) const FIXED_SOURCE_NAMES: &[&str] = &[
    "node.lock",
    "profile.db",
    "profile.db-wal",
    "profile.db-shm",
    "identity.keystore",
    "account.keystore",
    "names.log",
    "ratchet.sessions",
    "received.log",
    "listen-port",
    "privacy.policy",
    "peer-proofs",
    "peer-routes",
    "sent-manifest-scopes.log",
    "profile.db-journal",
    "received_files.log",
    "channel.senders",
    "channel.custody",
    "profiles.log",
    "recalls.log",
    "attachment-consent.log",
    "file-consent-controls.log",
    "recovery-origin.log",
    "recovery-contacts.log",
    "recovery-sent-ids.log",
    "trust-cards.log",
    "account-profiles.log",
    "account-suppression.log",
    "account-file-proofs.log",
    "account-access.log",
    "account-binding-transitions.log",
];

const ACCOUNT_LOG_PREFIXES: &[&str] = &[
    "favorites",
    "account-sync-policy",
    "account-devices",
    "channel-migration",
    "account-preferences",
    "account-documents",
];

fn account_suffix(name: &str, prefix: &str, suffix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix('-'))
        .and_then(|rest| rest.strip_suffix(suffix))
        .is_some_and(|id| id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Reserve domain names across retained accounts without interpreting the name
/// as a cryptographic identity or choosing an account from a source locator.
pub(super) fn reserved_sidecar_name(leaf: &std::ffi::OsStr) -> bool {
    let Some(name) = leaf.to_str() else {
        return false;
    };
    let name = name.to_lowercase();
    let reaction = name
        .strip_suffix("-reactions.log")
        .and_then(|prefix| prefix.rsplit_once('.'))
        .is_some_and(|(stem, id)| {
            !stem.is_empty() && id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())
        });
    let delivery_or_opaque = [
        ".delivery-owner",
        ".delivery-transactions",
        ".delivery-outbox",
        ".opaque-receive",
    ]
    .iter()
    .any(|suffix| {
        name.strip_suffix(suffix)
            .is_some_and(|stem| !stem.is_empty())
    });
    FIXED_SOURCE_NAMES.contains(&name.as_str())
        || reaction
        || delivery_or_opaque
        || ACCOUNT_LOG_PREFIXES
            .iter()
            .any(|prefix| account_suffix(&name, prefix, ".log"))
}

pub(super) fn reserved_directory_name(leaf: &std::ffi::OsStr) -> bool {
    let Some(name) = leaf.to_str() else {
        return false;
    };
    let name = name.to_lowercase();
    name == "media"
        || account_suffix(&name, "favorites", "")
        || account_suffix(&name, "account-document-blobs", "")
}

/// Reject authority leaves before compatibility constructors acquire a lock:
/// a caller's invalid source may itself be the existing node.lock file.
pub(crate) fn validate_authority_leaves(events: &Path, sent: &Path) -> Result<(), LogError> {
    for path in [events, sent] {
        let leaf = path
            .file_name()
            .ok_or_else(|| invalid("source must name a file"))?;
        if [
            "node.lock",
            "profile.db",
            "profile.db-wal",
            "profile.db-shm",
            "profile.db-journal",
        ]
        .iter()
        .any(|name| {
            leaf.to_str()
                .is_some_and(|leaf| leaf.to_lowercase() == *name)
        }) {
            return Err(invalid(
                "participant source collides with profile authority",
            ));
        }
    }
    Ok(())
}

/// Validate exact explicit paths before any legacy writer is opened. Reject
/// shared sidecar collisions rather than silently redirecting a supplied source.
pub(crate) fn paths(
    lease: &mut ProfileStorageLease,
    account: &AccountPublic,
    events: &Path,
    sent: &Path,
) -> Result<ParticipantPaths, LogError> {
    validate_authority_leaves(events, sent)?;
    let events = lease.claim_source(events)?;
    if events.parent() != Some(lease.canonical_root()) {
        return Err(invalid(
            "event source parent differs from primary profile root",
        ));
    }
    let sent = lease.claim_source(sent)?;
    let portable_alias = events.parent() == sent.parent()
        && events
            .file_name()
            .and_then(|leaf| leaf.to_str())
            .zip(sent.file_name().and_then(|leaf| leaf.to_str()))
            .is_some_and(|(left, right)| left.to_lowercase() == right.to_lowercase());
    if events == sent || portable_alias || publication_files::same_file(&events, &sent)? {
        return Err(invalid("event and sent sources collide"));
    }
    let root = lease.canonical_root();
    let account_id = account.account_id();
    let mut owned: Vec<PathBuf> = FIXED_SOURCE_NAMES
        .iter()
        .map(|name| root.join(name))
        .collect();
    for prefix in ACCOUNT_LOG_PREFIXES {
        owned.push(root.join(format!("{prefix}-{account_id}.log")));
    }
    owned.push(events.with_extension(format!("{account_id}-reactions.log")));
    for suffix in [
        ".delivery-owner",
        ".delivery-transactions",
        ".delivery-outbox",
        ".opaque-receive",
    ] {
        let mut path = events.as_os_str().to_owned();
        path.push(suffix);
        owned.push(path.into());
    }
    let directories = [
        root.join("media"),
        root.join(format!("favorites-{account_id}")),
        root.join(format!("account-document-blobs-{account_id}")),
    ];
    for source in [&events, &sent] {
        let primary_reserved =
            source.parent() == Some(root) && source.file_name().is_some_and(reserved_sidecar_name);
        let managed_directory = source
            .strip_prefix(root)
            .ok()
            .and_then(|relative| relative.components().next())
            .is_some_and(|component| reserved_directory_name(component.as_os_str()));
        if primary_reserved
            || managed_directory
            || owned.iter().any(|path| portable_path_alias(path, source))
            || directories.iter().any(|path| source.starts_with(path))
        {
            return Err(invalid(
                "participant source collides with an owned profile source",
            ));
        }
        // An external source parent also has its own fixed lock/DB namespace.
        if source.file_name().is_some_and(|leaf| {
            [
                "node.lock",
                "profile.db",
                "profile.db-wal",
                "profile.db-shm",
                "profile.db-journal",
            ]
            .iter()
            .any(|name| leaf == *name)
        }) {
            return Err(invalid(
                "participant source collides with profile authority",
            ));
        }
    }
    let mut unique = std::collections::HashSet::new();
    for path in &owned {
        if !unique.insert(path) {
            return Err(invalid("participant logical source paths collide"));
        }
        publication_files::inspect_regular(path)?;
        for source in [&events, &sent] {
            if publication_files::same_file(source, path)? {
                return Err(invalid(
                    "participant source aliases an owned profile source",
                ));
            }
        }
    }
    for directory in &directories {
        inspect_directory(directory)?;
        inspect_directory(&directory.join("media"))?;
        publication_files::inspect_regular(&directory.join("key.salt"))?;
        publication_files::inspect_regular(&directory.join("media/key.salt"))?;
    }
    Ok(ParticipantPaths { events, sent })
}

fn portable_path_alias(left: &Path, right: &Path) -> bool {
    left == right
        || (left.parent() == right.parent()
            && left
                .file_name()
                .and_then(|leaf| leaf.to_str())
                .zip(right.file_name().and_then(|leaf| leaf.to_str()))
                .is_some_and(|(left, right)| left.to_lowercase() == right.to_lowercase()))
}

fn inspect_directory(path: &Path) -> Result<(), LogError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || std::fs::canonicalize(path)? != path {
                return Err(invalid("profile media directory is redirected"));
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                if metadata.file_attributes() & 0x400 != 0 {
                    return Err(invalid("profile media directory is a reparse point"));
                }
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn locators(root: &Path, paths: &ParticipantPaths) -> [Vec<u8>; 2] {
    [&paths.events, &paths.sent]
        .map(|path| manifest::encode(path.strip_prefix(root).unwrap_or(path)))
}

pub(crate) fn binding(identity: &PublicIdentity) -> Vec<u8> {
    // The explicit physical namespace is shared with the existing relay profile;
    // mutable accounts and logical participant/relay roles never enter it.
    let mut binding = b"DeviceProfileV1".to_vec();
    binding.extend_from_slice(&identity.ed25519_pub);
    binding.extend_from_slice(&identity.x25519_pub);
    binding
}

pub(crate) fn open_existing(
    lease: &ProfileStorageLease,
    owner: &ParticipantOwner,
    password: &str,
) -> Result<Option<EncryptedDatabase>, LogError> {
    let path = lease.canonical_root().join("profile.db");
    if !publication_files::inspect_regular(&path)? {
        require_absent_companions(lease.canonical_root())?;
        return Ok(None);
    }
    let database = EncryptedDatabase::open(&path, password, &binding(&owner.identity))?;
    validate(
        &database,
        &locators(lease.canonical_root(), &owner.paths),
        &owner.account,
    )?;
    Ok(Some(database))
}

fn require_absent_companions(root: &Path) -> Result<(), LogError> {
    for name in ["profile.db-wal", "profile.db-shm", "profile.db-journal"] {
        match std::fs::symlink_metadata(root.join(name)) {
            Ok(_) => {
                return Err(invalid(
                    "missing profile main has unowned companion authority",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Only called after the actual legacy owners validated their sources and
/// recovered delivery/signed transitions under this lease. No source stubs or
/// history-domain tables are created in this metadata-only slice.
pub(crate) fn bootstrap(
    lease: &ProfileStorageLease,
    owner: &ParticipantOwner,
    password: &str,
    legacy_previous: Option<&str>,
) -> Result<EncryptedDatabase, LogError> {
    let root = lease.canonical_root();
    require_absent_companions(root)?;
    let path = root.join("profile.db");
    if path.try_exists()? {
        return Err(invalid("participant authority appeared during bootstrap"));
    }
    let stage = tempfile::Builder::new()
        .prefix(".participant-control-")
        .tempdir_in(root)?;
    let candidate = stage.path().join("candidate.db");
    let binding = binding(&owner.identity);
    let database = EncryptedDatabase::open(&candidate, password, &binding)?;
    let locators = locators(root, &owner.paths);
    database.transaction(|tx| {
        tx.execute_batch(SCHEMA)?;
        tx.execute(
            "INSERT INTO participant_profile VALUES(1,1,'participant-v1',?1,NULL,0,?2)",
            params![owner.account.ed25519_pub.as_slice(), legacy_previous],
        )?;
        tx.execute(
            "INSERT INTO participant_account_namespaces VALUES(?1,?2)",
            params![
                owner.account.account_id(),
                owner.account.ed25519_pub.as_slice()
            ],
        )?;
        if let Some(previous) = legacy_previous {
            tx.execute(
                "INSERT OR IGNORE INTO participant_account_namespaces VALUES(?1,NULL)",
                [previous],
            )?;
        }
        for (domain, locator) in locators.iter().enumerate() {
            tx.execute(
                "INSERT INTO participant_sources VALUES(?1,?2)",
                params![domain as i64, locator],
            )?;
        }
        Ok(())
    })?;
    database.checkpoint_and_close()?;
    let verified = EncryptedDatabase::open(&candidate, password, &binding)?;
    validate(&verified, &locators, &owner.account)?;
    verified.checkpoint_and_close()?;
    super::publication::cut("participant-before-publication")?;
    publication_files::publish_absent(&candidate, &path)?;
    publication_files::sync_parent(&path)?;
    super::publication::cut("participant-after-publication")?;
    EncryptedDatabase::open(&path, password, &binding)
}

struct ParticipantControlRow {
    version: i64,
    role: String,
    active_key: Vec<u8>,
    next_key: Option<Vec<u8>>,
    phase: i64,
    legacy_previous_id: Option<String>,
}

fn state(
    database: &EncryptedDatabase,
) -> Result<(AccountPublic, Option<AccountPublic>, Option<String>), LogError> {
    database.read(|connection| {
        manifest::validate_ddl(connection, SCHEMA)?;
        let row = connection.query_row(
            "SELECT version,role,account,next_account,phase,bootstrap_previous FROM participant_profile WHERE singleton=1", [],
            |row| Ok(ParticipantControlRow {
                version: row.get(0)?, role: row.get(1)?, active_key: row.get(2)?, next_key: row.get(3)?,
                phase: row.get(4)?, legacy_previous_id: row.get(5)?,
            }))?;
        let ParticipantControlRow { version, role, active_key: account, next_key: next, phase, legacy_previous_id: bootstrap } = row;
        if version != 1 || role != "participant-v1" || (phase == 0) != next.is_none() || !(0..=1).contains(&phase) {
            return Err(invalid("incompatible participant profile authority"));
        }
        let account = AccountPublic { ed25519_pub: account.try_into().map_err(|_| invalid("invalid active account"))? };
        let next = next.map(|key| key.try_into().map(|ed25519_pub| AccountPublic { ed25519_pub }).map_err(|_| invalid("invalid next account"))).transpose()?;
        if next.as_ref() == Some(&account) { return Err(invalid("invalid account adoption")); }
        if bootstrap.as_ref().is_some_and(|id| id.len()!=32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) || *id == account.account_id()) || (bootstrap.is_some() && next.is_some()) {
            return Err(invalid("invalid legacy participant adoption intent"));
        }
        if let Some(previous) = &bootstrap {
            let retained: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account_id=?1)", [previous], |r| r.get(0))?;
            if !retained { return Err(invalid("legacy participant adoption namespace is missing")); }
        }
        Ok((account,next,bootstrap))
    })
}

fn validate(
    database: &EncryptedDatabase,
    locators: &[Vec<u8>; 2],
    loaded: &AccountPublic,
) -> Result<(), LogError> {
    let (active, next, _) = state(database)?;
    if loaded != &active && next.as_ref() != Some(loaded) {
        return Err(invalid("unprepared participant account change"));
    }
    database.read(|connection| {
        let count: i64 =
            connection.query_row("SELECT count(*) FROM participant_sources", [], |r| r.get(0))?;
        if count != 2 {
            return Err(invalid("missing participant source layout"));
        }
        for (domain, locator) in locators.iter().enumerate() {
            let actual: Vec<u8> = connection.query_row(
                "SELECT locator FROM participant_sources WHERE domain=?1",
                [domain as i64],
                |r| r.get(0),
            )?;
            if &actual != locator {
                return Err(invalid("participant source layout changed"));
            }
        }
        let active_known: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account=?1)",
            [active.ed25519_pub.as_slice()],
            |r| r.get(0),
        )?;
        if !active_known {
            return Err(invalid("missing participant account namespace"));
        }
        Ok(())
    })
}

pub(crate) fn previous_account(
    database: &EncryptedDatabase,
    loaded: &AccountPublic,
) -> Result<Option<String>, LogError> {
    let (active, next, bootstrap) = state(database)?;
    if loaded == &active {
        Ok(bootstrap)
    } else if next.as_ref() == Some(loaded) {
        Ok(Some(active.account_id()))
    } else {
        Err(invalid("unprepared participant account change"))
    }
}

pub(crate) fn prepare_adoption(
    database: &EncryptedDatabase,
    previous: &AccountPublic,
    next: &AccountPublic,
) -> Result<(), LogError> {
    let (active, pending, bootstrap) = state(database)?;
    if bootstrap.is_some()
        || &active != previous
        || previous == next
        || pending.as_ref().is_some_and(|value| value != next)
    {
        return Err(invalid("incompatible participant account adoption"));
    }
    database.transaction(|tx| {
        tx.execute(
            "UPDATE participant_profile SET next_account=?1,phase=1 WHERE singleton=1",
            [next.ed25519_pub.as_slice()],
        )?;
        Ok(())
    })
}

/// Called only after the existing delivery and signed-transition owners have
/// recovered the exact loaded key. Old key cancels; exact next key completes.
pub(crate) fn finish_adoption(
    database: &EncryptedDatabase,
    loaded: &AccountPublic,
) -> Result<(), LogError> {
    let (active, next, bootstrap) = state(database)?;
    if loaded != &active && next.as_ref() != Some(loaded) {
        return Err(invalid("unprepared participant account change"));
    }
    if next.is_none() && bootstrap.is_none() {
        return Ok(());
    }
    database.transaction(|tx| {
        // A pre-SQL security rekey may retain only this account's hashed ID.
        // The authenticated loaded key can fill that exact unknown namespace,
        // but must never replace an already-known conflicting public key.
        tx.execute(
            "INSERT INTO participant_account_namespaces VALUES(?1,?2) \
             ON CONFLICT(account_id) DO UPDATE SET account=excluded.account \
             WHERE participant_account_namespaces.account IS NULL",
            params![loaded.account_id(), loaded.ed25519_pub.as_slice()],
        )?;
        let matching: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM participant_account_namespaces WHERE account_id=?1 AND account=?2)",
            params![loaded.account_id(), loaded.ed25519_pub.as_slice()],
            |row| row.get(0),
        )?;
        if !matching {
            return Err(invalid("conflicting participant account namespace"));
        }
        tx.execute("UPDATE participant_profile SET account=?1,next_account=NULL,phase=0,bootstrap_previous=NULL WHERE singleton=1", [loaded.ed25519_pub.as_slice()])?;
        Ok(())
    })
}
