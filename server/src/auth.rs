use anyhow::Context;
use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::distr::Uniform;
use rand::RngExt;
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


/// JWT claims (refresh token).
#[derive(Debug, Serialize, Deserialize)]
pub struct RefreshClaims {
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
    /// Token type: must be "refresh"
    pub typ: String,
    /// Session id (rotation / revocation)
    pub jti: String,
}

/// Short-lived signed token to download/preview a конкретный file_id without Authorization header.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileDlClaims {
    pub uid: i64,
    pub file_id: i64,
    pub token_version: i64,
    pub iat: i64,
    pub exp: i64,
    pub iss: String,
    pub typ: String, // "file"
}

pub fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Current UTC timestamp. PostgreSQL TIMESTAMPTZ-compatible.
pub fn now_iso() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
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


fn refresh_ttl_secs() -> i64 {
    std::env::var("REFRESH_TOKEN_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(60 * 60 * 24 * 30) // 30d
}

fn file_dl_ttl_secs() -> i64 {
    std::env::var("FILE_DL_TTL_SECS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| *v > 0 && *v <= 60 * 60)
        .unwrap_or(60) // 60s
}

fn secret_key_bytes() -> anyhow::Result<Vec<u8>> {
    let key = std::env::var("SECRET_KEY").context("SECRET_KEY env var is required")?;
    // Минимально разумная длина для HMAC ключа
    if key.len() < 32 {
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

/// Decode token and validate signature + issuer, but **do not** validate exp.
///
/// Нужно для мягкого продления сессии (refresh), чтобы пользователь не
/// вылетал на логин при долгом афк.
pub fn decode_claims_allow_expired(token: &str) -> anyhow::Result<Claims> {
    let key = secret_key_bytes()?;

    let mut v = Validation::new(Algorithm::HS256);
    v.validate_exp = false;

    let iss = issuer();
    v.set_issuer(&[iss.as_str()]);

    let data = decode::<Claims>(token, &DecodingKey::from_secret(&key), &v)?;
    Ok(data.claims)
}



pub fn create_refresh_token(username: &str, token_version: i64, jti: &str) -> anyhow::Result<String> {
    let iat = now_unix();
    let exp = iat + refresh_ttl_secs();
    let claims = RefreshClaims {
        sub: username.to_string(),
        token_version,
        iat,
        exp,
        iss: issuer(),
        typ: "refresh".to_string(),
        jti: jti.to_string(),
    };

    let key = secret_key_bytes()?;
    Ok(encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&key),
    )?)
}

pub fn decode_refresh_claims(token: &str) -> anyhow::Result<RefreshClaims> {
    let key = secret_key_bytes()?;
    let mut v = Validation::new(Algorithm::HS256);
    v.validate_exp = true;
    let iss = issuer();
    v.set_issuer(&[iss.as_str()]);
    let data = decode::<RefreshClaims>(token, &DecodingKey::from_secret(&key), &v)?;
    if data.claims.typ != "refresh" {
        anyhow::bail!("invalid token type");
    }
    Ok(data.claims)
}

pub fn create_file_download_token(user_id: i64, file_id: i64, token_version: i64) -> anyhow::Result<(String, i64)> {
    let iat = now_unix();
    let ttl = file_dl_ttl_secs();
    let exp = iat + ttl;
    let claims = FileDlClaims {
        uid: user_id,
        file_id,
        token_version,
        iat,
        exp,
        iss: issuer(),
        typ: "file".to_string(),
    };

    let key = secret_key_bytes()?;
    let tok = encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(&key),
    )?;
    Ok((tok, ttl))
}

pub fn decode_file_download_claims(token: &str) -> anyhow::Result<FileDlClaims> {
    let key = secret_key_bytes()?;
    let mut v = Validation::new(Algorithm::HS256);
    v.validate_exp = true;
    let iss = issuer();
    v.set_issuer(&[iss.as_str()]);
    let data = decode::<FileDlClaims>(token, &DecodingKey::from_secret(&key), &v)?;
    if data.claims.typ != "file" {
        anyhow::bail!("invalid token type");
    }
    Ok(data.claims)
}


pub fn normalize_username(input: &str) -> Option<String> {
    let username = input.trim();
    if username.is_empty() {
        return None;
    }

    let len = username.chars().count();
    if !(2..=32).contains(&len) {
        return None;
    }

    if username.chars().any(|c| c.is_control()) {
        return None;
    }

    Some(username.to_string())
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut salt_bytes = [0u8; 16];
    rand::rng().fill(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes).map_err(|e| anyhow::anyhow!(e.to_string()))?;
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
    let mut rng = rand::rng();
    let dist = Uniform::new(0u32, 1_000_000u32).unwrap();
    format!("{:06}", rng.sample(dist))
}

/// Generate backup codes for 2FA account recovery
/// Returns 10 codes in format: XXXX-XXXX-XXXX (48 bits each)
pub fn generate_2fa_backup_codes() -> Vec<String> {
    let mut rng = rand::rng();
    let dist = Uniform::new(0u64, 281_474_976_710_656u64).unwrap(); // 2^48
    
    (0..10)
        .map(|_| {
            let code = rng.sample(dist);
            format!("{:012X}", code)
                .chars()
                .collect::<Vec<_>>()
                .chunks(4)
                .map(|chunk| chunk.iter().collect::<String>())
                .collect::<Vec<_>>()
                .join("-")
        })
        .collect()
}

/// Verify backup code and mark as used
/// Codes should be stored as hashes in DB, never in plaintext
pub fn verify_backup_code(code: &str, stored_hash: &str) -> bool {
    let code_hash = sha256_hex(code);
    code_hash == stored_hash
}

/// Generate session ID for tracking user sessions
pub fn generate_session_id() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::rng();
    let mut id = [0u8; 16];
    rng.fill(&mut id);
    hex::encode(id)
}

pub fn sha256_hex(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    hex::encode(h.finalize())
}

/// Constant-time string comparison to prevent timing attacks
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        // Still do a dummy comparison to avoid timing leak
        let _ = verify_password("dummy", "");
        return false;
    }
    
    let mut result: u32 = 0;
    for (x, y) in a.bytes().zip(b.bytes()) {
        result |= (x ^ y) as u32;
    }
    result == 0
}

/// Timing-safe password verification with dummy Argon2 for non-existent users
pub fn verify_password_timing_safe(password: &str, stored_hash: Option<&str>) -> bool {
    match stored_hash {
        Some(hash) => {
            let Ok(parsed) = PasswordHash::new(hash) else {
                // Hash parsing failed, still do dummy verification
                let _ = verify_password("dummy", "");
                return false;
            };
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        },
        None => {
            // User doesn't exist, still perform Argon2 to equalize timing
            let dummy_hash = "$argon2id$v=19$m=19456,t=2,p=1$5pslEHwf7TvJPJfYiJU9sQ$wKzJpqEqlHnWQfcXlb0H5oDu+LJ6PO+HO5WdmxIyGyU";
            let _ = verify_password("dummy", dummy_hash);
            false
        }
    }
}
