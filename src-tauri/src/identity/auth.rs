use argon2::{
    password_hash::{rand_core::OsRng, SaltString},
    Argon2, PasswordHash, PasswordHasher, PasswordVerifier,
};

pub struct AuthManager;

impl AuthManager {
    pub fn hash_password(password: &str) -> Result<String, Box<dyn std::error::Error>> {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = Argon2::default();
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
        let argon2 = Argon2::default();
        let result = argon2.verify_password(password.as_bytes(), &parsed_hash);
        Ok(result.is_ok())
    }
}
