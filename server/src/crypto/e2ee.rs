use chacha20poly1305::aead::{Aead, KeyInit};
use x25519_dalek::{StaticSecret, PublicKey as X25519PublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use rand::RngExt;
use aes_gcm::{Aes256Gcm, Key, Nonce};

pub fn generate_room_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    rand::rng().fill(&mut key);
    key
}

pub fn encrypt_room_key(master_key: &[u8; 32], room_key: &[u8; 32]) -> (Vec<u8>, [u8; 12]) {
    let cipher_key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(cipher_key);
    
    let mut nonce = [0u8; 12];
    rand::rng().fill(&mut nonce);
    
    let ciphertext = cipher
        .encrypt(Nonce::from_slice(&nonce), room_key.as_ref())
        .expect("Encryption failed");
        
    (ciphertext, nonce)
}

pub fn decrypt_room_key(master_key: &[u8; 32], ciphertext: &[u8], nonce: &[u8; 12]) -> [u8; 32] {
    let cipher_key = Key::<Aes256Gcm>::from_slice(master_key);
    let cipher = Aes256Gcm::new(cipher_key);
    
    let pt = cipher
        .decrypt(Nonce::from_slice(nonce), ciphertext)
        .expect("Decryption failed");
        
    let mut room_key = [0u8; 32];
    room_key.copy_from_slice(&pt);
    room_key
}

// ==================== Base64 Utilities ====================
mod b64 {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    #[inline]
    fn decode_byte(b: u8) -> Option<u8> {
        match b {
            b'A'..=b'Z' => Some(b - b'A'),
            b'a'..=b'z' => Some(b - b'a' + 26),
            b'0'..=b'9' => Some(b - b'0' + 52),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }

    pub fn decode(input: &str) -> Option<Vec<u8>> {
        let mut bits: u32 = 0;
        let mut depth: u32 = 0;
        let mut out = Vec::new();

        for &b in input.as_bytes() {
            let val = decode_byte(b)?;
            bits = (bits << 6) | val as u32;
            depth += 6;
            if depth >= 8 {
                depth -= 8;
                out.push(((bits >> depth) & 0xff) as u8);
            }
        }
        Some(out)
    }

    pub fn encode(input: &[u8]) -> String {
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        let mut bits: u32 = 0;
        let mut depth: u32 = 0;

        for &b in input {
            bits = (bits << 8) | b as u32;
            depth += 8;
            while depth >= 6 {
                depth -= 6;
                out.push(CHARS[((bits >> depth) & 0x3f) as usize] as char);
            }
        }
        if depth > 0 {
            bits <<= 6 - depth;
            out.push(CHARS[((bits >> (6 - depth)) & 0x3f) as usize] as char);
        }
        out
    }
}

// ==================== X25519 Key Pair ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2eeKeyPair {
    pub public_key_b64: String,
    pub private_key_b64: Option<String>,
}

impl E2eeKeyPair {
    pub fn generate() -> Self {
        let mut secret_bytes = [0u8; 32];
        rand::rng().fill(&mut secret_bytes);
        secret_bytes[0] &= 248;
        secret_bytes[31] &= 127;
        secret_bytes[31] |= 64;
        let secret = StaticSecret::from(secret_bytes);
        let public = X25519PublicKey::from(&secret);
        let priv_b64 = b64::encode(&secret_bytes);
        let pub_b64 = b64::encode(public.as_bytes());
        Self {
            public_key_b64: pub_b64,
            private_key_b64: Some(priv_b64),
        }
    }

    pub fn from_public_b64(public_b64: &str) -> Option<Self> {
        let bytes = b64::decode(public_b64)?;
        if bytes.len() != 32 { return None; }
        let arr: [u8; 32] = bytes.try_into().ok()?;
        let _pub = X25519PublicKey::from(arr);
        let _ = _pub.as_bytes();
        Some(Self {
            public_key_b64: public_b64.to_string(),
            private_key_b64: None,
        })
    }

    pub fn from_private_b64(private_b64: &str) -> Option<Self> {
        let priv_bytes = b64::decode(private_b64)?;
        if priv_bytes.len() != 32 { return None; }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&priv_bytes);
        let secret = StaticSecret::from(arr);
        let public = X25519PublicKey::from(&secret);
        Some(Self {
            public_key_b64: b64::encode(public.as_bytes()),
            private_key_b64: Some(private_b64.to_string()),
        })
    }

    pub fn to_x25519_public(&self) -> Option<X25519PublicKey> {
        let bytes = b64::decode(&self.public_key_b64)?;
        let arr: [u8; 32] = bytes.try_into().ok()?;
        Some(X25519PublicKey::from(arr))
    }

    pub fn to_x25519_secret(&self) -> Option<StaticSecret> {
        let priv_bytes = b64::decode(self.private_key_b64.as_ref()?)?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&priv_bytes);
        Some(StaticSecret::from(arr))
    }

    pub fn diffie_hellman(&self, other_public: &X25519PublicKey) -> Option<[u8; 32]> {
        let secret = self.to_x25519_secret()?;
        let shared = secret.diffie_hellman(other_public);
        Some(shared.to_bytes())
    }

    pub fn fingerprint(&self) -> String {
        let bytes = b64::decode(&self.public_key_b64).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    }
}

// ==================== E2EE Message ====================
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2eeRecipientKey {
    pub ss: String,
    pub iv: String,
    pub ct: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2eeMessage {
    pub v: u8,
    pub sender: i64,
    pub device_id: String,
    pub epub: String,
    pub iv: String,
    pub payload: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<HashMap<String, HashMap<String, E2eeRecipientKey>>>,
}

impl E2eeMessage {
    pub fn decrypt(&self, my_key: &E2eeKeyPair, my_device_id: &str) -> Result<String, String> {
        if self.v != 1 {
            return Err(format!("Unsupported E2EE version: {}", self.v));
        }

        if let Some(ref keys) = self.keys {
            if let Some(device_keys) = keys.get(&self.sender.to_string()) {
                if let Some(rec_key) = device_keys.get(my_device_id) {
                    return Self::decrypt_with_shared(&self.epub, rec_key, my_key);
                }
            }
            for device_keys in keys.values() {
                if let Some(rec_key) = device_keys.get(my_device_id) {
                    return Self::decrypt_with_shared(&self.epub, rec_key, my_key);
                }
            }
        }

        Err("Message not encrypted for this device.".to_string())
    }

    fn decrypt_with_shared(
        epub_b64: &str,
        rec_key: &E2eeRecipientKey,
        my_key: &E2eeKeyPair,
    ) -> Result<String, String> {
        let epub_bytes = match b64::decode(epub_b64) {
            Some(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => return Err("Invalid epub".to_string()),
        };

        let shared = match my_key.diffie_hellman(&X25519PublicKey::from(epub_bytes)) {
            Some(s) => s,
            None => return Err("DH failed".to_string()),
        };

        let hk = hkdf::Hkdf::<Sha256>::new(Some(b"laberry-recv-key"), &shared);
        let mut recv_key = [0u8; 32];
        hk.expand(b"laberry-recv-key", &mut recv_key)
            .map_err(|e| format!("HKDF expand: {}", e))?;

        let iv_bytes = b64::decode(&rec_key.iv).ok_or("Invalid iv")?;
        let ct_bytes = b64::decode(&rec_key.ct).ok_or("Invalid ct")?;

        let cipher = chacha20poly1305::ChaCha20Poly1305::new_from_slice(&recv_key)
            .map_err(|e| format!("ChaCha20Poly1305 new: {}", e))?;
        let nonce = chacha20poly1305::Nonce::from_slice(&iv_bytes);

        let pt = cipher
            .decrypt(nonce, ct_bytes.as_ref())
            .map_err(|e| format!("ChaCha20 decrypt: {}", e))?;

        String::from_utf8(pt).map_err(|e| format!("UTF-8: {}", e))
    }

    pub fn is_e2ee_content(content: &str) -> bool {
        content.starts_with("[[e2ee:v")
    }

    pub fn from_content(content: &str) -> Option<Self> {
        if !Self::is_e2ee_content(content) { return None; }
        let prefix = "[[e2ee:v";
        if let Some(inner) = content.strip_prefix(prefix) {
            if let Some(pipe_pos) = inner.find('|') {
                let version_str = &inner[..pipe_pos];
                if let Ok(v) = version_str.parse::<u8>() {
                    let json = inner[pipe_pos + 1..].strip_suffix("]]")?;
                    return Self::from_json(json).ok().map(|mut msg| { msg.v = v; msg });
                }
            }
        }
        None
    }

    pub fn to_content(&self) -> String {
        if let Ok(json) = Self::to_json(self) {
            format!("[[e2ee:v{}|{}]]", self.v, json)
        } else {
            String::new()
        }
    }

    fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

// ==================== Hex helper ====================
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{:02x}", b))
            .collect()
    }
}

// ==================== Tests ====================
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keypair_generate_and_serialize() {
        let key = E2eeKeyPair::generate();
        assert!(!key.public_key_b64.is_empty());
        assert!(key.private_key_b64.is_some());
        
        let restored = E2eeKeyPair::from_private_b64(key.private_key_b64.as_ref().unwrap()).unwrap();
        assert_eq!(key.public_key_b64, restored.public_key_b64);
    }

    #[test]
    fn test_keypair_fingerprint_deterministic() {
        let key = E2eeKeyPair::generate();
        let fp1 = key.fingerprint();
        let fp2 = key.fingerprint();
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64);
    }

    #[test]
    fn test_dh_exchange() {
        let alice = E2eeKeyPair::generate();
        let bob = E2eeKeyPair::generate();

        let alice_pub = alice.to_x25519_public().unwrap();
        let bob_pub = bob.to_x25519_public().unwrap();

        let alice_shared = alice.diffie_hellman(&bob_pub).unwrap();
        let bob_shared = bob.diffie_hellman(&alice_pub).unwrap();

        assert_eq!(alice_shared, bob_shared, "DH shared secrets must match");
    }

    #[test]
    fn test_content_roundtrip() {
        let msg = E2eeMessage {
            v: 1,
            sender: 1,
            device_id: "test".to_string(),
            epub: b64::encode(&[0u8; 32]),
            iv: b64::encode(&[0u8; 12]),
            payload: b64::encode(b"test"),
            keys: None,
        };

        let content = msg.to_content();
        assert!(content.starts_with("[[e2ee:v1|"));
        assert!(content.ends_with("]]"));

        let parsed = E2eeMessage::from_content(&content);
        assert!(parsed.is_some());
        assert_eq!(parsed.unwrap().sender, 1);
    }

    #[test]
    fn test_from_public_only() {
        let key = E2eeKeyPair::generate();
        let public_only = E2eeKeyPair::from_public_b64(&key.public_key_b64).unwrap();
        assert_eq!(public_only.public_key_b64, key.public_key_b64);
        assert!(public_only.private_key_b64.is_none());
    }
}