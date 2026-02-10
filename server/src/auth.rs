use anyhow::Context;
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::{distributions::Uniform, Rng};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

/// JWT claims (access token).
///
/// Важно: любые изменения полей здесь — это изменения контракта токена.
#[derive(Debug, Serialize, Deserialize)]
pub struct Claims {
    /// Subject: username
    pub sub: String,
    /// Token invalidation version stored in DB
    pub token_version: i64,
    /// Issued at (unix seconds)
    pub iat: i64,
    /// Expiration time (unix seconds)
    pub exp: i64,
    /// Issuer
    pub iss: String,
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Unix time (seconds) as string — подходит для SQLite TEXT.
pub fn now_iso() -> String {
    now_unix().to_string()
}

fn issuer() -> String {
    // issuer не критичен для совместимости, поэтому допустим дефолт
    std::env::var("JWT_ISSUER").unwrap_or_else(|_| "laberry".to_string())
}

fn access_ttl_secs() -> i64 {
    // TTL можно конфигурировать, но дефолт стабильный
    std::env::var("ACCESS_TOKEN_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(60 * 60) // 1h
}

fn secret_key_bytes() -> anyhow::Result<Vec<u8>> {
    let key = std::env::var("SECRET_KEY").context("SECRET_KEY env var is required")?;
    // Минимально разумная длина для HMAC ключа
    if key.as_bytes().len() < 32 {
        anyhow::bail!("SECRET_KEY must be at least 32 bytes long");
    }
    Ok(key.into_bytes())
}

pub fn create_access_token(username: &str, token_version: i64) -> anyhow::Result<String> {
    let iat = now_unix();
    let exp = iat + access_ttl_secs();
    let claims = Claims {
        sub: username.to_string(),
        token_version,
        iat,
        exp,
        iss: issuer(),
    };

    let key = secret_key_bytes()?;
    Ok(encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&key),
    )?)
}

/// Decode + validate access token.
/// Returns (username, token_version).
pub fn decode_username(token: &str) -> anyhow::Result<(String, i64)> {
    let key = secret_key_bytes()?;

    let mut v = Validation::new(Algorithm::HS256);
    // exp включён по умолчанию, но оставим явно
    v.validate_exp = true;

    // issuer — валидируем (JWT_ISSUER или дефолт "laberry")
    let iss = issuer();
    v.set_issuer(&[iss.as_str()]);

    let data = decode::<Claims>(token, &DecodingKey::from_secret(&key), &v)?;
    Ok((data.claims.sub, data.claims.token_version))
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut rand::thread_rng());
    let argon2 = Argon2::default();

    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!(e.to_string()))?
        .to_string();

    Ok(hash)
}

pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(stored_hash) else {
        return false;
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub fn generate_2fa_code_6() -> String {
    let mut rng = rand::thread_rng();
    let dist = Uniform::new_inclusive(0u32, 999_999u32);
    format!("{:06}", rng.sample(dist))
}

pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}
