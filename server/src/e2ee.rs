// ======================================================
// 🔐 E2EE Security: Key Validation, Pinning & Verification
// ======================================================

use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// JWK (JSON Web Key) validation for E2EE public keys
/// Supports P-256 (ES256) ECDH keys
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JwkKey {
    // Required
    pub kty: String,      // "EC" for elliptic curve
    pub crv: String,      // "P-256"
    pub x: String,        // x coordinate (base64url)
    pub y: String,        // y coordinate (base64url)
    
    // Optional
    pub use_: Option<String>,    // "enc" for encryption
    pub key_ops: Option<Vec<String>>,
    pub alg: Option<String>,     // "ECDH-ES+HKDF-256"
    pub kid: Option<String>,     // Key ID
}

impl JwkKey {
    /// Validate JWK structure and format
    pub fn validate(&self) -> anyhow::Result<()> {
        // Check key type
        if self.kty != "EC" {
            anyhow::bail!("Invalid key type: {}, expected 'EC'", self.kty);
        }

        // Check curve
        if self.crv != "P-256" {
            anyhow::bail!("Invalid curve: {}, expected 'P-256'", self.crv);
        }

        // Check x and y coordinates
        if self.x.is_empty() || self.y.is_empty() {
            anyhow::bail!("Missing x or y coordinate");
        }

        // Validate base64url encoding (should only contain valid base64url chars)
        for coord in &[&self.x, &self.y] {
            if !coord.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                anyhow::bail!("Invalid base64url encoding in coordinates");
            }
        }

        Ok(())
    }

    /// Compute SHA-256 fingerprint of JWK for key pinning
    /// Returns hex-encoded fingerprint
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        // Include canonical fields
        hasher.update(format!("{}|{}|{}|{}", self.kty, self.crv, self.x, self.y));
        hex::encode(hasher.finalize())
    }

    /// Parse JWK from JSON string
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let jwk: JwkKey = serde_json::from_str(json)?;
        jwk.validate()?;
        Ok(jwk)
    }
}

/// Key pinning tracker - stores trusted fingerprints
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyPin {
    pub user_id: i64,
    pub device_id: String,
    pub fingerprint: String,
    pub created_at: i64,
    pub last_verified_at: i64,
}

impl KeyPin {
    /// Check if key fingerprint has significantly changed
    /// (More than just minor format differences)
    pub fn is_significant_change(old_fp: &str, new_fp: &str) -> bool {
        old_fp != new_fp
    }
}

/// E2EE message envelope validation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2eeEnvelope {
    pub alg: String,
    pub sender: i64,
    pub sender_key: String,
    pub ephemeral: String,
    pub iv: String,
    pub ct: String,
    pub keys: HashMap<String, HashMap<String, HashMap<String, String>>>,
}

impl E2eeEnvelope {
    /// Parse and validate E2EE envelope
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        let envelope: E2eeEnvelope = serde_json::from_str(json)?;
        envelope.validate()?;
        Ok(envelope)
    }

    /// Validate envelope structure
    pub fn validate(&self) -> anyhow::Result<()> {
        // Check algorithm version
        if self.alg != "LB-E2EE-v1" {
            anyhow::bail!("Unsupported E2EE algorithm: {}", self.alg);
        }

        // Check required fields
        if self.sender <= 0 {
            anyhow::bail!("Invalid sender ID");
        }

        if self.sender_key.is_empty() || self.ephemeral.is_empty() {
            anyhow::bail!("Missing sender or ephemeral key");
        }

        if self.iv.is_empty() || self.ct.is_empty() {
            anyhow::bail!("Missing IV or ciphertext");
        }

        if self.keys.is_empty() {
            anyhow::bail!("No recipients in envelope");
        }

        // Validate base64url fields
        for field in &[&self.iv, &self.ct] {
            if !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
                anyhow::bail!("Invalid base64url encoding");
            }
        }

        Ok(())
    }
}

/// Device key registration validation
pub struct DeviceKeyValidator;

impl DeviceKeyValidator {
    /// Validate device registration request
    pub fn validate_registration(
        device_id: &str,
        public_jwk: &str,
        label: &Option<String>,
    ) -> anyhow::Result<()> {
        // Validate device_id format (UUID or random string)
        if device_id.is_empty() || device_id.len() > 255 {
            anyhow::bail!("Invalid device_id length");
        }

        // Only allow alphanumeric, hyphens, underscores
        if !device_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            anyhow::bail!("Invalid device_id format");
        }

        // Validate JWK
        let jwk = JwkKey::from_json(public_jwk)?;

        // Validate label if provided
        if let Some(lbl) = label {
            if lbl.len() > 512 {
                anyhow::bail!("Label too long (max 512 chars)");
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jwk_validation() {
        let valid_jwk = r#"{
            "kty": "EC",
            "crv": "P-256",
            "x": "WKn-ZIGevcwGIyyrzFoZNBdaq9_TsqzGl96oc0CWuis",
            "y": "y77t-RvAHRKTsSGdIYUfweuOvwrvDD-Q3Hv5J0fSKbE"
        }"#;

        let jwk = JwkKey::from_json(valid_jwk).unwrap();
        assert_eq!(jwk.kty, "EC");
        assert_eq!(jwk.crv, "P-256");
    }

    #[test]
    fn test_jwk_validation_fails() {
        let invalid_jwk = r#"{"kty": "RSA", "crv": "P-256", "x": "test", "y": "test"}"#;
        assert!(JwkKey::from_json(invalid_jwk).is_err());
    }

    #[test]
    fn test_fingerprint() {
        let jwk = JwkKey {
            kty: "EC".to_string(),
            crv: "P-256".to_string(),
            x: "test_x".to_string(),
            y: "test_y".to_string(),
            use_: None,
            key_ops: None,
            alg: None,
            kid: None,
        };
        
        let fp1 = jwk.fingerprint();
        let fp2 = jwk.fingerprint();
        assert_eq!(fp1, fp2); // Deterministic
    }

    #[test]
    fn test_envelope_validation() {
        let envelope = r#"{
            "alg": "LB-E2EE-v1",
            "sender": 1,
            "sender_key": "key",
            "ephemeral": "ephemeral",
            "iv": "aGVsbG8",
            "ct": "d29ybGQ",
            "keys": {"1": {"device1": {"iv": "a", "ct": "b"}}}
        }"#;

        let env = E2eeEnvelope::from_json(envelope).unwrap();
        assert_eq!(env.alg, "LB-E2EE-v1");
    }
}

// Add hex encoding helper
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}
