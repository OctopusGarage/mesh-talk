//! Versioned account authentication policy carried only by private account access.
//! Operations are synchronous disk/KDF work: invoke on an admitted blocking worker.
//! No startup/idle tasks; records are bounded to 4 KiB and ten recovery verifiers.
//! Replay and throttling are local, rollbackable state, not distributed guarantees.
//! Password plus file compromise exposes the seed; this is not offline MFA.
//! Unknown versions and incomplete enabled policies must never become disabled policy.
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountSecurity {
    pub version: u8,
    pub generation: u64,
    pub totp: Option<TotpEnrollment>,
    #[serde(default)]
    pub consumed_recovery: Vec<[u8; 32]>,
    pub last_step: Option<u64>,
    pub failures: u8,
    pub blocked_until: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TotpEnrollment {
    /// RFC 6238 HMAC-SHA1 key, encrypted by the containing credential record.
    pub secret: [u8; 20],
    /// SHA-256 verifiers of independently generated high-entropy recovery codes.
    pub recovery_hashes: Vec<[u8; 32]>,
}

/// Private wire policy deliberately excludes device-local verification history.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountSecurityPolicy {
    pub version: u8,
    pub generation: u64,
    pub totp: Option<TotpEnrollment>,
}
impl AccountSecurityPolicy {
    pub fn into_state(self) -> AccountSecurity {
        AccountSecurity {
            version: self.version,
            generation: self.generation,
            totp: self.totp,
            ..Default::default()
        }
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || self.totp.as_ref().is_some_and(|v| {
                self.generation == 0
                    || v.secret == [0; 20]
                    || v.recovery_hashes.len() > 10
                    || v.recovery_hashes
                        .iter()
                        .enumerate()
                        .any(|(i, h)| *h == [0; 32] || v.recovery_hashes[..i].contains(h))
            })
        {
            return Err("Invalid account security policy");
        }
        Ok(())
    }
}
impl AccountSecurity {
    pub fn into_policy(self) -> AccountSecurityPolicy {
        AccountSecurityPolicy {
            version: self.version,
            generation: self.generation,
            totp: self.totp,
        }
    }
}

impl Drop for TotpEnrollment {
    fn drop(&mut self) {
        self.secret.zeroize();
        self.recovery_hashes.zeroize();
    }
}

impl AccountSecurity {
    /// Legacy absence is disabled. Explicit records require a known version.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.consumed_recovery.len() > 10 {
            return Err("Invalid consumed recovery history");
        }
        if self.version != 1 {
            return Err("Unsupported account security policy");
        }
        if let Some(enrollment) = &self.totp {
            if self.generation == 0
                || enrollment.secret == [0; 20]
                || enrollment.recovery_hashes.len() > 10
            {
                return Err("Invalid account security policy");
            }
            for (index, hash) in enrollment.recovery_hashes.iter().enumerate() {
                if *hash == [0; 32] || enrollment.recovery_hashes[..index].contains(hash) {
                    return Err("Invalid account recovery verifiers");
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_policy_never_silently_disables_authentication() {
        for version in [0, 2, 255] {
            assert!(
                AccountSecurity {
                    version,
                    ..Default::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            AccountSecurity {
                version: 1,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn enabled_policy_requires_generation_and_valid_bounded_recovery_set() {
        let mut policy = AccountSecurity {
            version: 1,
            generation: 1,
            consumed_recovery: vec![],
            last_step: None,
            failures: 0,
            blocked_until: 0,
            totp: Some(TotpEnrollment {
                secret: [1; 20],
                recovery_hashes: vec![[2; 32]],
            }),
        };
        assert!(policy.validate().is_ok());
        policy.generation = 0;
        assert!(policy.validate().is_err());
        policy.generation = 1;
        policy.totp.as_mut().unwrap().recovery_hashes.push([2; 32]);
        assert!(policy.validate().is_err());
        policy.totp.as_mut().unwrap().recovery_hashes = vec![[3; 32]; 11];
        assert!(policy.validate().is_err());
    }
}

use super::{errors::IdentityError, manager::IdentityManager};
use ring::{
    digest, hmac,
    rand::{SecureRandom, SystemRandom},
};
const PATH: &str = "meta/account-security.enc";
fn error(message: impl ToString) -> IdentityError {
    IdentityError::StorageError(message.to_string())
}

/// Caller-owned enrollment. Dropping cancels; no policy changes before confirmation.
/// Never log or serialize this value. Expiry uses caller-supplied trusted Unix time.
#[derive(Clone)]
pub struct PendingEnrollment {
    owner: String,
    generation: u64,
    expires: u64,
    pub secret: zeroize::Zeroizing<[u8; 20]>,
    pub recovery_codes: zeroize::Zeroizing<Vec<String>>,
}
fn otp(secret: &[u8], step: u64, digits: u32) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let bytes = tag.as_ref();
    let offset = (bytes[19] & 15) as usize;
    let value = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) & 0x7fff_ffff;
    format!(
        "{:0width$}",
        value % 10u32.pow(digits),
        width = digits as usize
    )
}
fn equal(a: &[u8], b: &[u8]) -> bool {
    // HMAC verification supplies the maintained constant-time comparison primitive.
    let key = hmac::Key::new(hmac::HMAC_SHA256, b"mesh-talk policy comparison");
    hmac::verify(&key, a, hmac::sign(&key, b).as_ref()).is_ok()
}
impl IdentityManager {
    pub(super) fn security_read(
        &self,
        user: &str,
        password: &str,
    ) -> Result<AccountSecurity, IdentityError> {
        self.authenticate_user(user, password)?;
        let path = self.file_manager.user_data_path(user).join(PATH);
        match std::fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AccountSecurity {
                version: 1,
                ..Default::default()
            }),
            Err(e) => Err(error(e)),
            Ok(meta) => {
                if !meta.is_file() || meta.len() > 4096 {
                    return Err(error("Invalid security record"));
                }
                let state: AccountSecurity = self
                    .file_manager
                    .read_encrypted_file(user, PATH, password)
                    .map_err(error)?;
                state.validate().map_err(error)?;
                Ok(state)
            }
        }
    }
    fn security_write(
        &self,
        user: &str,
        password: &str,
        state: &AccountSecurity,
    ) -> Result<(), IdentityError> {
        self.file_manager
            .write_encrypted_file(user, PATH, state, password)
            .map_err(error)
    }
    fn security_write_if(
        &self,
        user: &str,
        password: &str,
        state: &AccountSecurity,
        authorize: impl FnOnce(
            &mut dyn FnMut() -> Result<(), IdentityError>,
        ) -> Result<(), IdentityError>,
    ) -> Result<(), IdentityError> {
        let mut prepared = Some(
            self.file_manager
                .prepare_encrypted_write(user, PATH, state, password)
                .map_err(error)?,
        );
        let mut published = None;
        authorize(&mut || {
            published = Some(
                prepared
                    .take()
                    .ok_or_else(|| error("Publication already attempted"))?
                    .publish()
                    .map_err(error)?,
            );
            Ok(())
        })?;
        published
            .ok_or_else(|| error("Publication not authorized"))?
            .finish();
        Ok(())
    }
    /// Password-authenticated policy snapshot. Contains secrets: private authenticated transport only.
    pub fn export_account_security(
        &self,
        user: &str,
        password: &str,
    ) -> Result<AccountSecurity, IdentityError> {
        let _lock = self.account_login_namespace_lock()?;
        self.security_read(user, password)
    }
    pub fn begin_account_totp(
        &self,
        user: &str,
        password: &str,
        now: u64,
    ) -> Result<PendingEnrollment, IdentityError> {
        let _lock = self.account_login_namespace_lock()?;
        let state = self.security_read(user, password)?;
        let mut secret = zeroize::Zeroizing::new([0; 20]);
        SystemRandom::new()
            .fill(secret.as_mut())
            .map_err(|_| error("Random generation failed"))?;
        let mut codes = zeroize::Zeroizing::new(Vec::new());
        for _ in 0..10 {
            let mut bytes = zeroize::Zeroizing::new([0u8; 32]);
            SystemRandom::new()
                .fill(bytes.as_mut())
                .map_err(|_| error("Random generation failed"))?;
            codes.push(bytes.iter().map(|b| format!("{b:02x}")).collect());
        }
        Ok(PendingEnrollment {
            owner: user.into(),
            generation: state.generation,
            expires: now.saturating_add(600),
            secret,
            recovery_codes: codes,
        })
    }
    pub fn confirm_account_totp(
        &self,
        user: &str,
        password: &str,
        pending: PendingEnrollment,
        code: &str,
        now: u64,
    ) -> Result<(), IdentityError> {
        self.confirm_account_totp_if(user, password, pending, code, now, |commit| commit())
    }
    pub fn confirm_account_totp_if(
        &self,
        user: &str,
        password: &str,
        pending: PendingEnrollment,
        code: &str,
        now: u64,
        authorize: impl FnOnce(
            &mut dyn FnMut() -> Result<(), IdentityError>,
        ) -> Result<(), IdentityError>,
    ) -> Result<(), IdentityError> {
        let _lock = self.account_login_namespace_lock()?;
        let mut state = self.security_read(user, password)?;
        if pending.owner != user
            || pending.generation != state.generation
            || now >= pending.expires
            || !equal(
                code.as_bytes(),
                otp(pending.secret.as_ref(), now / 30, 6).as_bytes(),
            )
        {
            return Err(error("Invalid enrollment proof"));
        }
        if state.totp.is_some() {
            return Err(error("Use rotate_account_totp with current proof"));
        }
        state.generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| error("Generation exhausted"))?;
        state.totp = Some(TotpEnrollment {
            secret: *pending.secret,
            recovery_hashes: pending
                .recovery_codes
                .iter()
                .map(|c| {
                    digest::digest(&digest::SHA256, c.as_bytes())
                        .as_ref()
                        .try_into()
                        .unwrap()
                })
                .collect(),
        });
        state.consumed_recovery.clear();
        state.last_step = Some(now / 30);
        self.security_write_if(user, password, &state, authorize)
    }
    /// Atomic persisted verification. Success consumes this step or recovery code.
    pub fn verify_account_totp(
        &self,
        user: &str,
        password: &str,
        proof: &str,
        now: u64,
    ) -> Result<(), IdentityError> {
        let _lock = self.account_login_namespace_lock()?;
        let mut state = self.security_read(user, password)?;
        if state.totp.is_none() {
            return Ok(());
        }
        let valid = consume_proof(&mut state, proof, now)?;
        self.security_write(user, password, &state)?;
        if valid {
            Ok(())
        } else {
            Err(error("Invalid authentication proof"))
        }
    }
}

#[cfg(test)]
mod persisted_tests {
    use super::*;
    #[test]
    fn policy_import_preserves_local_verifier_history() {
        let dir = tempfile::tempdir().unwrap();
        let manager = IdentityManager::new(crate::storage::file_manager::FileManager::new(
            dir.path().into(),
        ));
        manager.register_user("alice", "password").unwrap();
        let current = AccountSecurity {
            version: 1,
            generation: 1,
            failures: 4,
            blocked_until: 3030,
            consumed_recovery: vec![],
            last_step: Some(100),
            totp: Some(TotpEnrollment {
                secret: [1; 20],
                recovery_hashes: vec![],
            }),
        };
        manager
            .security_write("alice", "password", &current)
            .unwrap();
        let incoming = AccountSecurity {
            version: 1,
            generation: 2,
            failures: 0,
            blocked_until: 0,
            consumed_recovery: vec![],
            last_step: Some(999),
            totp: Some(TotpEnrollment {
                secret: [1; 20],
                recovery_hashes: vec![],
            }),
        };
        manager
            .import_account_security("alice", "password", 1, incoming)
            .unwrap();
        let actual = manager
            .export_account_security("alice", "password")
            .unwrap();
        assert_eq!(actual.failures, 4);
        assert_eq!(actual.blocked_until, 3030);
        assert_eq!(actual.last_step, Some(100));
    }

    #[test]
    fn same_seed_import_cannot_resurrect_consumed_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let manager = IdentityManager::new(crate::storage::file_manager::FileManager::new(
            dir.path().into(),
        ));
        manager.register_user("alice", "password").unwrap();
        let pending = manager
            .begin_account_totp("alice", "password", 3000)
            .unwrap();
        let recovery = pending.recovery_codes[0].clone();
        let code = otp(pending.secret.as_ref(), 100, 6);
        manager
            .confirm_account_totp("alice", "password", pending, &code, 3000)
            .unwrap();
        let policy = serde_json::to_vec(
            &manager
                .export_account_security("alice", "password")
                .unwrap()
                .into_policy(),
        )
        .unwrap();
        manager
            .verify_account_totp("alice", "password", &recovery, 3000)
            .unwrap();
        assert_eq!(
            policy,
            serde_json::to_vec(
                &manager
                    .export_account_security("alice", "password")
                    .unwrap()
                    .into_policy()
            )
            .unwrap()
        );
        let mut incoming = manager
            .export_account_security("alice", "password")
            .unwrap()
            .into_policy()
            .into_state();
        incoming.generation += 1;
        manager
            .import_account_security("alice", "password", 1, incoming)
            .unwrap();
        assert!(
            manager
                .verify_account_totp("alice", "password", &recovery, 3030)
                .is_err()
        );
        let mut incoming = manager
            .export_account_security("alice", "password")
            .unwrap()
            .into_policy()
            .into_state();
        incoming.generation += 1;
        incoming.totp.as_mut().unwrap().secret = [9; 20];
        manager
            .import_account_security("alice", "password", 2, incoming)
            .unwrap();
        assert!(
            manager
                .export_account_security("alice", "password")
                .unwrap()
                .consumed_recovery
                .is_empty()
        );
    }

    #[test]
    fn rejected_prepared_enrollment_preserves_disabled_record() {
        let dir = tempfile::tempdir().unwrap();
        let manager = IdentityManager::new(crate::storage::file_manager::FileManager::new(
            dir.path().into(),
        ));
        manager.register_user("alice", "password").unwrap();
        let pending = manager
            .begin_account_totp("alice", "password", 3000)
            .unwrap();
        let code = otp(pending.secret.as_ref(), 100, 6);
        assert!(
            manager
                .confirm_account_totp_if("alice", "password", pending, &code, 3000, |_| Err(error(
                    "session retired"
                )))
                .is_err()
        );
        assert!(
            manager
                .export_account_security("alice", "password")
                .unwrap()
                .totp
                .is_none()
        );
    }

    #[test]
    fn official_sha1_vectors() {
        for (time, expected) in [
            (59, "94287082"),
            (1111111109, "07081804"),
            (1111111111, "14050471"),
            (1234567890, "89005924"),
            (2000000000, "69279037"),
            (20000000000, "65353130"),
        ] {
            assert_eq!(otp(b"12345678901234567890", time / 30, 8), expected);
            assert_eq!(otp(b"12345678901234567890", time / 30, 6), &expected[2..]);
        }
    }
    #[test]
    fn failed_confirmation_unknown_version_and_restart_throttle() {
        let dir = tempfile::tempdir().unwrap();
        let fm = crate::storage::file_manager::FileManager::new(dir.path().into());
        let manager = IdentityManager::new(fm.clone());
        manager.register_user("alice", "password").unwrap();
        let pending = manager
            .begin_account_totp("alice", "password", 3000)
            .unwrap();
        assert!(
            manager
                .confirm_account_totp("alice", "password", pending, "bad", 3000)
                .is_err()
        );
        assert!(
            manager
                .export_account_security("alice", "password")
                .unwrap()
                .totp
                .is_none()
        );
        let pending = manager
            .begin_account_totp("alice", "password", 3000)
            .unwrap();
        let code = otp(pending.secret.as_ref(), 100, 6);
        manager
            .confirm_account_totp("alice", "password", pending, &code, 3000)
            .unwrap();
        for _ in 0..5 {
            assert!(
                manager
                    .verify_account_totp("alice", "password", "bad", 3000)
                    .is_err()
            );
        }
        let restarted = IdentityManager::new(fm.clone());
        assert!(
            restarted
                .verify_account_totp("alice", "password", &code, 3000)
                .is_err()
        );
        assert_eq!(
            restarted
                .export_account_security("alice", "password")
                .unwrap()
                .blocked_until,
            3030
        );
        fm.write_encrypted_file(
            "alice",
            PATH,
            &AccountSecurity {
                version: 255,
                ..Default::default()
            },
            "password",
        )
        .unwrap();
        assert!(
            restarted
                .export_account_security("alice", "password")
                .is_err()
        );
    }
    #[test]
    fn concurrent_recovery_has_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let manager = IdentityManager::new(crate::storage::file_manager::FileManager::new(
            dir.path().into(),
        ));
        manager.register_user("alice", "password").unwrap();
        let pending = manager
            .begin_account_totp("alice", "password", 3000)
            .unwrap();
        let recovery = pending.recovery_codes[0].clone();
        let code = otp(pending.secret.as_ref(), 100, 6);
        manager
            .confirm_account_totp("alice", "password", pending, &code, 3000)
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let manager = manager.clone();
                let recovery = recovery.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    manager
                        .verify_account_totp("alice", "password", &recovery, 3000)
                        .is_ok()
                })
            })
            .collect();
        assert_eq!(
            handles
                .into_iter()
                .filter_map(|h| h.join().ok())
                .filter(|v| *v)
                .count(),
            1
        );
        let pending = manager
            .begin_account_totp("alice", "password", 3030)
            .unwrap();
        let new_code = otp(pending.secret.as_ref(), 101, 6);
        manager
            .replace_account_totp(
                "alice",
                "password",
                &otp(
                    &manager
                        .export_account_security("alice", "password")
                        .unwrap()
                        .totp
                        .as_ref()
                        .unwrap()
                        .secret,
                    101,
                    6,
                ),
                Some((pending, &new_code)),
                3030,
            )
            .unwrap();
        manager
            .replace_account_totp(
                "alice",
                "password",
                &otp(
                    &manager
                        .export_account_security("alice", "password")
                        .unwrap()
                        .totp
                        .as_ref()
                        .unwrap()
                        .secret,
                    102,
                    6,
                ),
                None,
                3060,
            )
            .unwrap();
        assert!(
            manager
                .export_account_security("alice", "password")
                .unwrap()
                .totp
                .is_none()
        );
    }
    #[test]
    fn persisted_enrollment_replay_recovery_and_corruption() {
        let dir = tempfile::tempdir().unwrap();
        let manager = IdentityManager::new(crate::storage::file_manager::FileManager::new(
            dir.path().into(),
        ));
        manager.register_user("alice", "password").unwrap();
        assert!(
            manager
                .export_account_security("alice", "password")
                .unwrap()
                .totp
                .is_none()
        );
        let pending = manager
            .begin_account_totp("alice", "password", 3000)
            .unwrap();
        let code = otp(pending.secret.as_ref(), 100, 6);
        let recovery = pending.recovery_codes[0].clone();
        manager
            .confirm_account_totp("alice", "password", pending, &code, 3000)
            .unwrap();
        let code = otp(
            &manager
                .export_account_security("alice", "password")
                .unwrap()
                .totp
                .as_ref()
                .unwrap()
                .secret,
            101,
            6,
        );
        manager
            .verify_account_totp("alice", "password", &code, 3030)
            .unwrap();
        assert!(
            manager
                .verify_account_totp("alice", "password", &code, 3000)
                .is_err()
        );
        manager
            .verify_account_totp("alice", "password", &recovery, 3000)
            .unwrap();
        assert!(
            manager
                .verify_account_totp("alice", "password", &recovery, 3000)
                .is_err()
        );
        std::fs::write(
            manager.file_manager.user_data_path("alice").join(PATH),
            b"corrupt",
        )
        .unwrap();
        assert!(
            manager
                .export_account_security("alice", "password")
                .is_err()
        );
    }
}

fn consume_proof(
    state: &mut AccountSecurity,
    proof: &str,
    now: u64,
) -> Result<bool, IdentityError> {
    if proof.len() > 64 {
        return Err(error("Authentication proof too long"));
    }
    let Some(enrollment) = state.totp.as_mut() else {
        return Ok(true);
    };
    if now < state.blocked_until {
        return Err(error("Verification throttled"));
    }
    let step = now / 30;
    let matched = [step.saturating_sub(1), step, step.saturating_add(1)]
        .into_iter()
        .find(|s| {
            state.last_step.is_none_or(|last| *s > last)
                && equal(proof.as_bytes(), otp(&enrollment.secret, *s, 6).as_bytes())
        });
    let hash = digest::digest(&digest::SHA256, proof.as_bytes());
    let recovery = enrollment
        .recovery_hashes
        .iter()
        .position(|h| equal(h, hash.as_ref()) && !state.consumed_recovery.contains(h));
    let valid = matched.is_some() || recovery.is_some();
    if let Some(s) = matched {
        state.last_step = Some(s);
    } else if let Some(i) = recovery {
        state.consumed_recovery.push(enrollment.recovery_hashes[i]);
    }
    if valid {
        state.failures = 0;
        state.blocked_until = 0;
    } else {
        state.failures = state.failures.saturating_add(1);
        if state.failures >= 5 {
            state.blocked_until = now.saturating_add(30);
        }
    }

    Ok(valid)
}

impl IdentityManager {
    /// Atomic current-proof authorized disable or confirmed replacement.
    pub fn replace_account_totp(
        &self,
        user: &str,
        password: &str,
        proof: &str,
        replacement: Option<(PendingEnrollment, &str)>,
        now: u64,
    ) -> Result<(), IdentityError> {
        self.replace_account_totp_if(user, password, proof, replacement, now, |commit| commit())
    }
    pub fn replace_account_totp_if(
        &self,
        user: &str,
        password: &str,
        proof: &str,
        replacement: Option<(PendingEnrollment, &str)>,
        now: u64,
        authorize: impl FnOnce(
            &mut dyn FnMut() -> Result<(), IdentityError>,
        ) -> Result<(), IdentityError>,
    ) -> Result<(), IdentityError> {
        let _lock = self.account_login_namespace_lock()?;
        let mut state = self.security_read(user, password)?;
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| error("Generation exhausted"))?;
        if let Some((pending, code)) = &replacement
            && (pending.owner != user
                || pending.generation != state.generation
                || now >= pending.expires
                || !equal(
                    code.as_bytes(),
                    otp(pending.secret.as_ref(), now / 30, 6).as_bytes(),
                ))
        {
            return Err(error("Invalid enrollment proof"));
        }
        if !consume_proof(&mut state, proof, now)? {
            self.security_write_if(user, password, &state, authorize)?;
            return Err(error("Invalid authentication proof"));
        }
        state.totp = replacement.map(|(pending, _)| TotpEnrollment {
            secret: *pending.secret,
            recovery_hashes: pending
                .recovery_codes
                .iter()
                .map(|c| {
                    digest::digest(&digest::SHA256, c.as_bytes())
                        .as_ref()
                        .try_into()
                        .unwrap()
                })
                .collect(),
        });
        state.consumed_recovery.clear();
        state.generation = generation;
        state.last_step = state.totp.as_ref().map(|_| now / 30);
        self.security_write_if(user, password, &state, authorize)
    }
}

impl IdentityManager {
    /// Import from an already authenticated private account channel. The consumer
    /// must authenticate the cryptographic account identity before calling this;
    /// `user` is only the local storage namespace. CAS prevents stale overwrites.
    pub fn import_account_security(
        &self,
        user: &str,
        password: &str,
        expected_generation: u64,
        incoming: AccountSecurity,
    ) -> Result<(), IdentityError> {
        self.import_account_security_if(user, password, expected_generation, incoming, |commit| {
            commit()
        })
    }
    pub fn import_account_security_if(
        &self,
        user: &str,
        password: &str,
        expected_generation: u64,
        mut incoming: AccountSecurity,
        authorize: impl FnOnce(
            &mut dyn FnMut() -> Result<(), IdentityError>,
        ) -> Result<(), IdentityError>,
    ) -> Result<(), IdentityError> {
        let _lock = self.account_login_namespace_lock()?;
        let current = self.security_read(user, password)?;
        incoming.validate().map_err(error)?;
        if current.generation != expected_generation || incoming.generation <= current.generation {
            return Err(error("Stale security policy"));
        }
        // Remote verifier history is never authoritative for this device. Preserve
        // local throttling across policy replacement; replay belongs to the seed.
        incoming.failures = current.failures;
        incoming.blocked_until = current.blocked_until;
        incoming.consumed_recovery = match (&current.totp, &incoming.totp) {
            (Some(old), Some(new)) if equal(&old.secret, &new.secret) => {
                current.consumed_recovery.clone()
            }
            _ => vec![],
        };
        incoming.last_step = match (&current.totp, &incoming.totp) {
            (Some(old), Some(new)) if equal(&old.secret, &new.secret) => current.last_step,
            _ => None,
        };
        self.security_write_if(user, password, &incoming, authorize)
    }
}

#[cfg(test)]
mod wire_policy_tests {
    use super::*;
    #[test]
    fn policy_wire_excludes_local_attempt_state_and_rejects_unknown_fields() {
        let policy = AccountSecurity {
            version: 1,
            failures: 5,
            blocked_until: 999,
            last_step: Some(20),
            ..Default::default()
        }
        .into_policy();
        let bytes = serde_json::to_vec(&policy).unwrap();
        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(!text.contains("failures"));
        assert!(!text.contains("blocked_until"));
        assert!(!text.contains("last_step"));
        let decoded: AccountSecurityPolicy = serde_json::from_slice(&bytes).unwrap();
        assert!(decoded.validate().is_ok());
        assert!(decoded.totp.is_none());
        assert!(
            serde_json::from_str::<AccountSecurityPolicy>(
                r#"{"version":1,"generation":0,"totp":null,"failures":0}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<AccountSecurityPolicy>(
                r#"{"version":255,"generation":0,"totp":null}"#
            )
            .unwrap()
            .validate()
            .is_err()
        );
    }
}
