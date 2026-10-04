//! Historical routing hints only. Every use requires separate verified proofs and
//! full Ed25519/X25519 transport pinning; an IP address proves no identity.
use crate::identity::device::PublicIdentity;
use crate::storage::encryption::{
    decrypt_data, encrypt_data, generate_salt, EncryptionKey, NONCE_SIZE, SALT_SIZE,
};
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    net::IpAddr,
    path::{Path, PathBuf},
};
const MAGIC: &[u8; 6] = b"MTRTE1";
const MAX_BYTES: u64 = 1_048_576;
const HEADER: usize = MAGIC.len() + SALT_SIZE + NONCE_SIZE;
fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid routing cache")
}
fn bad<T>(_: T) -> io::Error {
    invalid()
}
fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_BYTES)
        .reject_trailing_bytes()
}
fn check(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() => Ok(true),
        Ok(_) => Err(invalid()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Route {
    public: PublicIdentity,
    ip: IpAddr,
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    routes: Vec<Route>,
}
pub(crate) struct RouteCache {
    path: PathBuf,
    salt: [u8; SALT_SIZE],
    key: EncryptionKey,
    routes: Vec<Route>,
}
impl RouteCache {
    pub(crate) fn open(path: &Path, password: &str) -> io::Result<Self> {
        if !check(path)? {
            let salt = generate_salt();
            let cache = Self {
                path: path.into(),
                salt,
                key: EncryptionKey::from_password(password, &salt).map_err(bad)?,
                routes: Vec::new(),
            };
            cache.save(&[])?;
            return Ok(cache);
        }
        let file = File::open(path)?;
        if !file.metadata()?.is_file() {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        if bytes.len() < HEADER + 16
            || bytes.len() as u64 > MAX_BYTES
            || &bytes[..MAGIC.len()] != MAGIC
        {
            return Err(invalid());
        }
        let salt = bytes[MAGIC.len()..MAGIC.len() + SALT_SIZE]
            .try_into()
            .map_err(bad)?;
        let nonce = bytes[MAGIC.len() + SALT_SIZE..HEADER]
            .try_into()
            .map_err(bad)?;
        let key = EncryptionKey::from_password(password, &salt).map_err(bad)?;
        let plaintext = decrypt_data(&bytes[HEADER..], &nonce, &key).map_err(bad)?;
        let snapshot: Snapshot = codec().deserialize(&plaintext).map_err(bad)?;
        if snapshot.version != 1
            || snapshot.routes.len() > 1024
            || snapshot
                .routes
                .windows(2)
                .any(|w| w[0].public.user_id() >= w[1].public.user_id())
        {
            return Err(invalid());
        }
        Ok(Self {
            path: path.into(),
            salt,
            key,
            routes: snapshot.routes,
        })
    }
    pub(crate) fn record(&mut self, public: &PublicIdentity, ip: IpAddr) -> io::Result<()> {
        let mut next = self.routes.clone();
        let id = public.user_id();
        let route = Route {
            public: public.clone(),
            ip,
        };
        match next.binary_search_by(|r| r.public.user_id().cmp(&id)) {
            Ok(i) => {
                if next[i].public != *public {
                    return Err(invalid());
                }
                if next[i] == route {
                    return Ok(());
                }
                next[i] = route;
            }
            Err(i) => {
                if next.len() == 1024 {
                    return Err(invalid());
                }
                next.insert(i, route);
            }
        }
        self.save(&next)?;
        self.routes = next;
        Ok(())
    }
    pub(crate) fn routes(&self) -> Vec<(PublicIdentity, IpAddr)> {
        self.routes
            .iter()
            .map(|r| (r.public.clone(), r.ip))
            .collect()
    }
    fn save(&self, routes: &[Route]) -> io::Result<()> {
        let plaintext = codec().serialize(&(1u32, routes)).map_err(bad)?;
        let (ciphertext, nonce) = encrypt_data(&plaintext, &self.key).map_err(bad)?;
        if HEADER + ciphertext.len() > MAX_BYTES as usize {
            return Err(invalid());
        }
        let mut bytes = MAGIC.to_vec();
        bytes.extend(self.salt);
        bytes.extend(nonce);
        bytes.extend(ciphertext);
        replace(&self.path, &bytes)
    }
}

fn replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    check(path)?;
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
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    check(path)?;
    temporary.persist(path).map_err(|e| e.error)?;
    if let Ok(directory) = File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}
pub(in crate::node) async fn stable_listener(
    directory: &Path,
) -> io::Result<tokio::net::TcpListener> {
    let path = directory.join("listen-port");
    let preferred = if check(&path)? {
        let mut bytes = Vec::new();
        File::open(&path)?.take(3).read_to_end(&mut bytes)?;
        let bytes: [u8; 2] = bytes.try_into().map_err(bad)?;
        let port = u16::from_be_bytes(bytes);
        if port == 0 {
            return Err(invalid());
        }
        port
    } else {
        0
    };
    let listener = match crate::transport::net::bind_dual_stack_listener(preferred).await {
        Ok(listener) => listener,
        Err(_) if preferred != 0 => {
            log::warn!("Preferred node listen port unavailable; selecting another port");
            crate::transport::net::bind_dual_stack_listener(0).await?
        }
        Err(e) => return Err(e),
    };
    replace(&path, &listener.local_addr()?.port().to_be_bytes())?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;
    #[test]
    fn encrypted_routes_reopen_exact_keys_noops_and_failed_save_preserve_memory() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("routes");
        let public = DeviceIdentity::generate().public();
        let ip = "127.0.0.1".parse().unwrap();
        let mut cache = RouteCache::open(&path, "pw").unwrap();
        cache.record(&public, ip).unwrap();
        let original = std::fs::read(&path).unwrap();
        cache.record(&public, ip).unwrap();
        assert_eq!(original, std::fs::read(&path).unwrap());
        assert_eq!(
            RouteCache::open(&path, "pw").unwrap().routes(),
            vec![(public.clone(), ip)]
        );
        assert!(RouteCache::open(&path, "wrong").is_err());
        let mut corrupt = original.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        std::fs::write(&path, corrupt).unwrap();
        assert!(RouteCache::open(&path, "pw").is_err());
        std::fs::write(&path, original).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        assert!(cache.record(&public, "127.0.0.2".parse().unwrap()).is_err());
        assert_eq!(cache.routes(), vec![(public, ip)]);
    }
}
