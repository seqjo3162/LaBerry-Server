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
