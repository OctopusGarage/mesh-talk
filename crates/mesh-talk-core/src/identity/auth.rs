use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};

pub struct AuthManager;

// Argon2 is one of the two KDFs that dominate the debug test suite. It is collapsed to a
// trivial cost in two TEST-ONLY situations: `cfg(test)` (true only when compiling THIS crate as
// a test target) and the `fast-test-kdf` feature (enabled exclusively from a downstream crate's
// `[dev-dependencies]`, e.g. src-tauri — Cargo does NOT activate it for normal `cargo build`/
// release builds). Release builds and the shipped app always get the strong production default;
// there is no way to enable the cheap path in a real build.
#[cfg(not(any(test, feature = "fast-test-kdf")))]
fn argon2() -> Argon2<'static> {
    Argon2::default()
}

#[cfg(any(test, feature = "fast-test-kdf"))]
fn argon2() -> Argon2<'static> {
    // Minimum values the argon2 crate accepts (1 lane, 1 pass, 8 KiB).
    let params = argon2::Params::new(argon2::Params::MIN_M_COST, 1, 1, None)
        .expect("minimal argon2 params are valid");
    Argon2::new(
        argon2::Algorithm::default(),
        argon2::Version::default(),
        params,
    )
}

impl AuthManager {
    pub fn hash_password(password: &str) -> Result<String, Box<dyn std::error::Error>> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = argon2();
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| format!("Failed to hash password: {}", e))?;
        Ok(password_hash.to_string())
    }

    pub fn verify_password(
        password: &str,
        hashed_password: &str,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let parsed_hash = PasswordHash::new(hashed_password)
            .map_err(|e| format!("Failed to parse password hash: {}", e))?;
        let argon2 = argon2();
        let result = argon2.verify_password(password.as_bytes(), &parsed_hash);
        Ok(result.is_ok())
    }
}
