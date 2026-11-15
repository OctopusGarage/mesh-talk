//! Historical routing hints only. Every use requires separate verified proofs and
//! full Ed25519/X25519 transport pinning; an IP address proves no identity.
use crate::identity::device::PublicIdentity;
use crate::storage::encryption::{
    EncryptionKey, NONCE_SIZE, SALT_SIZE, decrypt_data, encrypt_data, generate_salt,
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
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(not(test), derive(Clone))]
struct Route {
    public: PublicIdentity,
    ips: Vec<IpAddr>,
}
#[cfg(test)]
thread_local! {
    static ROUTE_CLONES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
impl Clone for Route {
    fn clone(&self) -> Self {
        ROUTE_CLONES.with(|count| count.set(count.get() + 1));
        Self {
            public: self.public.clone(),
            ips: self.ips.clone(),
        }
    }
}
#[derive(Serialize, Deserialize)]
struct LegacyRoute {
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
        let version = plaintext
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_le_bytes)
            .ok_or_else(invalid)?;
        let snapshot: Snapshot = match version {
            1 => {
                let (version, old): (u32, Vec<LegacyRoute>) =
                    codec().deserialize(&plaintext).map_err(bad)?;
                Snapshot {
                    version,
                    routes: old
                        .into_iter()
                        .map(|route| Route {
                            public: route.public,
                            ips: vec![route.ip],
                        })
                        .collect(),
                }
            }
            2 => codec().deserialize(&plaintext).map_err(bad)?,
            _ => return Err(invalid()),
        };
        if snapshot.version != version
            || snapshot.routes.len() > 1024
            || snapshot
                .routes
                .windows(2)
                .any(|w| w[0].public.user_id() >= w[1].public.user_id())
            || snapshot.routes.iter().any(|route| {
                route.ips.is_empty()
                    || route.ips.len() > 8
                    || route
                        .ips
                        .iter()
                        .enumerate()
                        .any(|(i, ip)| route.ips[..i].contains(ip))
            })
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
        let id = public.user_id();
        let position = self
            .routes
            .binary_search_by(|r| r.public.user_id().cmp(&id));
        match position {
            Ok(i) => {
                if self.routes[i].public != *public {
                    return Err(invalid());
                }
                if self.routes[i].ips.contains(&ip) {
                    return Ok(());
                }
            }
            Err(_) if self.routes.len() == 1024 => return Err(invalid()),
            Err(_) => {}
        }
        let mut next = self.routes.clone();
        match position {
            Ok(i) => {
                next[i].ips.push(ip);
                if next[i].ips.len() > 8 {
                    next[i].ips.remove(0);
                }
            }
            Err(i) => next.insert(
                i,
                Route {
                    public: public.clone(),
                    ips: vec![ip],
                },
            ),
        }
        self.save(&next)?;
        self.routes = next;
        Ok(())
    }
    pub(crate) fn routes(&self) -> Vec<(PublicIdentity, IpAddr)> {
        self.routes
            .iter()
            .flat_map(|r| r.ips.iter().map(|ip| (r.public.clone(), *ip)))
            .collect()
    }
    pub(crate) fn contains(&self, public: &PublicIdentity, ip: IpAddr) -> bool {
        self.routes
            .binary_search_by(|route| route.public.user_id().cmp(&public.user_id()))
            .ok()
            .is_some_and(|index| {
                self.routes[index].public == *public && self.routes[index].ips.contains(&ip)
            })
    }
    fn save(&self, routes: &[Route]) -> io::Result<()> {
        let plaintext = codec().serialize(&(2u32, routes)).map_err(bad)?;
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
    fn full_directory_noops_and_rejections_do_not_clone_routes() {
        let root = tempfile::tempdir().unwrap();
        let mut cache = RouteCache::open(&root.path().join("routes"), "pw").unwrap();
        let ip = "127.0.0.1".parse().unwrap();
        // Populate the bounded owner directly: no 1024 encrypted writes in setup.
        cache.routes = (0u32..1024)
            .map(|i| {
                let mut ed25519_pub = [0; 32];
                ed25519_pub[..4].copy_from_slice(&i.to_le_bytes());
                Route {
                    public: PublicIdentity {
                        ed25519_pub,
                        x25519_pub: [1; 32],
                    },
                    ips: vec![ip],
                }
            })
            .collect();
        cache.routes.sort_by_key(|route| route.public.user_id());
        let public = cache.routes[500].public.clone();
        ROUTE_CLONES.with(|count| count.set(0));
        cache.record(&public, ip).unwrap();
        let mut wrong_x = public.clone();
        wrong_x.x25519_pub[0] ^= 1;
        assert!(cache.record(&wrong_x, ip).is_err());
        assert!(
            cache
                .record(&DeviceIdentity::generate().public(), ip)
                .is_err()
        );
        assert_eq!(ROUTE_CLONES.with(|count| count.get()), 0);
        let next_ip = "127.0.0.2".parse().unwrap();
        cache.record(&public, next_ip).unwrap();
        assert_eq!(ROUTE_CLONES.with(|count| count.get()), 1024);
        let reopened = RouteCache::open(&root.path().join("routes"), "pw").unwrap();
        assert_eq!(reopened.routes.len(), 1024);
        assert!(reopened.contains(&public, next_ip));
        assert!(reopened.contains(&public, ip));
    }

    #[test]
    fn mutations_retain_last_eight_and_failed_save_retries_and_reopens() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("routes");
        let public = DeviceIdentity::generate().public();
        let mut cache = RouteCache::open(&path, "pw").unwrap();
        for i in 1..=9 {
            cache.record(&public, IpAddr::from([127, 0, 0, i])).unwrap();
        }
        let expected: Vec<_> = (2..=9)
            .map(|i| (public.clone(), IpAddr::from([127, 0, 0, i])))
            .collect();
        assert_eq!(cache.routes(), expected);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        let next = IpAddr::from([127, 0, 0, 10]);
        assert!(cache.record(&public, next).is_err());
        assert_eq!(cache.routes(), expected);
        fs::remove_dir(&path).unwrap();
        cache.record(&public, next).unwrap();
        assert_eq!(
            RouteCache::open(&path, "pw").unwrap().routes(),
            cache.routes()
        );
        assert!(!cache.contains(&public, IpAddr::from([127, 0, 0, 2])));
        assert!(cache.contains(&public, next));
    }

    #[test]
    fn retains_multiple_ips_for_one_identity_and_migrates_v1() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("routes");
        let public = DeviceIdentity::generate().public();
        let lan: IpAddr = "192.168.1.20".parse().unwrap();
        let vpn: IpAddr = "100.101.102.20".parse().unwrap();
        let salt = generate_salt();
        let key = EncryptionKey::from_password("pw", &salt).unwrap();
        let legacy = codec()
            .serialize(&(1u32, vec![(public.clone(), lan)]))
            .unwrap();
        let (ciphertext, nonce) = encrypt_data(&legacy, &key).unwrap();
        let mut bytes = MAGIC.to_vec();
        bytes.extend(salt);
        bytes.extend(nonce);
        bytes.extend(ciphertext);
        std::fs::write(&path, bytes).unwrap();

        let mut cache = RouteCache::open(&path, "pw").unwrap();
        cache.record(&public, vpn).unwrap();
        assert_eq!(
            cache.routes(),
            vec![(public.clone(), lan), (public.clone(), vpn)]
        );
        assert_eq!(
            RouteCache::open(&path, "pw").unwrap().routes(),
            cache.routes()
        );
    }
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
