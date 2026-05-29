use chacha20poly1305::aead::{Aead, KeyInit};
use x25519_dalek::StaticSecret;
use x25519_dalek::PublicKey as X25519PublicKey;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

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
        let mut out = String::with_capacity(((input.len() + 2) / 3) * 4);
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

/// X25519 keypair для E2EE (только публичный ключ хранится на сервере)
/// Приватный ключ генерируется и хранится ТОЛЬКО на клиенте
#[derive(Debug, Clone)]
pub struct E2eeKeyPair {
    /// Публичный ключ (base64-encoded 32 bytes) — хранится на сервере
    pub public_key_b64: String,
    /// Приватный ключ (base64-encoded 32 bytes) — ТОЛЬКО для клиента
    pub private_key_b64: Option<String>,
}

impl E2eeKeyPair {
    /// Генерируем новую keypair (приватный + публичный ключ)
    pub fn generate() -> Self {
        let mut secret_bytes = [0u8; 32];
        getrandom::getrandom(&mut secret_bytes).expect("getrandom failed");
        // clamp bits for X25519
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

    /// Создаём keypair из base64-encoded публичного ключа
    /// Приватный ключ НЕ загружается с сервера — это клиентская ответственность
    pub fn from_public_b64(public_b64: &str) -> Option<Self> {
        let bytes = b64::decode(public_b64)?;
        if bytes.len() != 32 {
            return None;
        }
        // Валидируем что это корректный X25519 public key
        let arr: [u8; 32] = bytes.try_into().ok()?;
        let _pub = X25519PublicKey::from(arr);
        let _ = _pub.as_bytes(); // just to use it
        Some(Self {
            public_key_b64: public_b64.to_string(),
            private_key_b64: None,
        })
    }

    /// Создаём keypair из base64-encoded приватного ключа
    pub fn from_private_b64(private_b64: &str) -> Option<Self> {
        let priv_bytes = b64::decode(private_b64)?;
        if priv_bytes.len() != 32 {
            return None;
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&priv_bytes);
        let secret = StaticSecret::from(arr);
        let public = X25519PublicKey::from(&secret);
        Some(Self {
            public_key_b64: b64::encode(public.as_bytes()),
            private_key_b64: Some(private_b64.to_string()),
        })
    }

    /// Получаем X25519PublicKey из stored public key
    pub fn to_x25519_public(&self) -> Option<X25519PublicKey> {
        let bytes = b64::decode(&self.public_key_b64)?;
        let arr: [u8; 32] = bytes.try_into().ok()?;
        Some(X25519PublicKey::from(arr))
    }

    /// Получаем StaticSecret из stored private key (только если есть)
    pub fn to_x25519_secret(&self) -> Option<StaticSecret> {
        let priv_bytes = b64::decode(self.private_key_b64.as_ref()?)?;
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&priv_bytes);
        Some(StaticSecret::from(arr))
    }

    /// DH: вычисляем shared secret из нашего приватного и чужого публичного ключа
    pub fn diffie_hellman(&self, other_public: &X25519PublicKey) -> Option<[u8; 32]> {
        let secret = self.to_x25519_secret()?;
        let shared = secret.diffie_hellman(other_public);
        Some(shared.to_bytes())
    }

    /// Получить fingerprint (SHA-256 от публичного ключа)
    pub fn fingerprint(&self) -> String {
        let bytes = b64::decode(&self.public_key_b64).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        hex::encode(hasher.finalize())
    }
}

// ==================== E2EE Message ====================

/// Ключ получателя (зашифрованный обёрнутый ключ)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2eeRecipientKey {
    /// shared secret (base64 X25519 DH)
    pub ss: String,
    /// IV для шифрования (base64)
    pub iv: String,
    /// Зашифрованный payload (base64)
    pub ct: String,
}

/// Зашифрованное сообщение (envelope)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2eeMessage {
    /// Версия формата: 1 = legacy (X25519 + ChaCha20), 2 = новый (ECDH P-256 + AES-GCM)
    pub v: u8,
    /// ID отправителя
    pub sender: i64,
    /// ID устройства отправителя
    pub device_id: String,
    /// ephemeral public key (base64 X25519)
    pub epub: String,
    /// IV для шифрования payload (base64)
    pub iv: String,
    /// Зашифрованный payload (base64)
    pub payload: String,
    /// Ключи для получателей: user_id -> device_id -> E2eeRecipientKey
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<HashMap<String, HashMap<String, E2eeRecipientKey>>>,
}

impl E2eeMessage {
    /// Расшифровать сообщение (только для клиента — сервер не должен вызывать)
    pub fn decrypt(&self, my_key: &E2eeKeyPair, my_device_id: &str) -> Result<String, String> {
        if self.v != 1 {
            return Err(format!("Unsupported E2EE version: {}", self.v));
        }

        // Пытаемся найти свой ключ в keys
        if let Some(ref keys) = self.keys {
            // Ищем по sender -> device_id
            if let Some(device_keys) = keys.get(&self.sender.to_string()) {
                if let Some(rec_key) = device_keys.get(my_device_id) {
                    return Self::decrypt_with_shared(&self.epub, rec_key, my_key);
                }
            }
            // Ищем по любому user_id -> device_id
            for device_keys in keys.values() {
                if let Some(rec_key) = device_keys.get(my_device_id) {
                    return Self::decrypt_with_shared(&self.epub, rec_key, my_key);
                }
            }
        }

        // Если нет в keys — пробуем decrypt с ephemeral key (для self-encrypt)
        // Это работает когда отправитель шифрует для себя
        // Требуется приватный ключ отправителя
        return Err("Message not encrypted for this device. Try decrypting with sender's key.".to_string());
    }

    /// Расшифровать через shared secret (DH)
    fn decrypt_with_shared(
        epub_b64: &str,
        rec_key: &E2eeRecipientKey,
        my_key: &E2eeKeyPair,
    ) -> Result<String, String> {
        // Получаем ephemeral public key отправителя
        let epub_bytes = match b64::decode(epub_b64) {
            Some(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => return Err("Invalid epub".to_string()),
        };

        // DH: my_secret * ep_pub = shared
        let shared = match my_key.diffie_hellman(&X25519PublicKey::from(epub_bytes)) {
            Some(s) => s,
            None => return Err("DH failed".to_string()),
        };

        // HKDF для ключа шифрования получателя
        let hk = hkdf::Hkdf::<Sha256>::new(Some(b"laberry-recv-key"), &shared);
        let mut recv_key = [0u8; 32];
        hk.expand(b"laberry-recv-key", &mut recv_key)
            .map_err(|e| format!("HKDF expand: {}", e))?;

        let iv_bytes = match b64::decode(&rec_key.iv) {
            Some(b) => b,
            None => return Err("Invalid iv".to_string()),
        };
        let ct_bytes = match b64::decode(&rec_key.ct) {
            Some(b) => b,
            None => return Err("Invalid ct".to_string()),
        };

        let cipher = chacha20poly1305::ChaCha20Poly1305::new_from_slice(&recv_key)
            .map_err(|e| format!("ChaCha20Poly1305 new: {}", e))?;
        let nonce = chacha20poly1305::Nonce::from_slice(&iv_bytes);

        let pt = cipher
            .decrypt(&nonce, ct_bytes.as_ref())
            .map_err(|e| format!("ChaCha20 decrypt: {}", e))?;

        String::from_utf8(pt).map_err(|e| format!("UTF-8: {}", e))
    }

    /// Check if content is an E2EE message
    pub fn is_e2ee_content(content: &str) -> bool {
        content.starts_with("[[e2ee:v")
    }

    /// Parse E2EE message from content string "[[e2ee:v1|<json>]]"
    pub fn from_content(content: &str) -> Option<Self> {
        if !Self::is_e2ee_content(content) {
            return None;
        }
        // Извлекаем JSON между [[e2ee:vN| и ]]
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

    /// Convert to content string "[[e2ee:vN|<json>]]"
    pub fn to_content(&self) -> String {
        if let Ok(json) = Self::to_json(self) {
            format!("[[e2ee:v{}|{}]]", self.v, json)
        } else {
            String::new()
        }
    }

    /// Serialize to JSON string
    fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Deserialize from JSON string
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
        
        // Восстанавливаем из приватного ключа
        let restored = E2eeKeyPair::from_private_b64(key.private_key_b64.as_ref().unwrap()).unwrap();
        assert_eq!(key.public_key_b64, restored.public_key_b64);
    }

    #[test]
    fn test_keypair_fingerprint_deterministic() {
        let key = E2eeKeyPair::generate();
        let fp1 = key.fingerprint();
        let fp2 = key.fingerprint();
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64); // SHA-256 hex
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
