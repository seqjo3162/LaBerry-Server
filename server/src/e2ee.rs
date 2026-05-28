use chacha20poly1305::aead::{Aead, KeyInit};
use crypto_box::{SecretKey, PublicKey};
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

// ==================== Account Key ====================

/// Аккаунт-ключ пользователя (единый для всех устройств)
/// Хранится как base64-encoded 32-byte seed в users.public_encryption_key
#[derive(Debug, Clone)]
pub struct AccountKey {
    seed: [u8; 32],
}

impl AccountKey {
    /// Создать новый аккаунт-ключ (случайный)
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("getrandom failed");
        Self { seed }
    }

    /// Создать из base64-encoded строки
    pub fn from_base64(b64: &str) -> Option<Self> {
        let bytes = b64::decode(b64)?;
        if bytes.len() != 32 {
            return None;
        }
        let mut seed = [0u8; 32];
        seed.copy_from_slice(&bytes);
        Some(Self { seed })
    }

    /// В base64
    pub fn to_base64(&self) -> String {
        b64::encode(&self.seed)
    }

    /// Получить fingerprint (SHA-256 от seed)
    pub fn fingerprint(&self) -> String {
        let mut hasher = Sha256::new();
        hasher.update(&self.seed);
        hex::encode(hasher.finalize())
    }

    /// Derive device-specific key: HKDF(account_seed, device_id) -> device_key
    pub fn derive_device_key(&self, device_id: &str) -> [u8; 32] {
        let salt = device_id.as_bytes();
        let hk = hkdf::Hkdf::<Sha256>::new(Some(salt), &self.seed);
        let mut okm = [0u8; 32];
        hk.expand(b"laberry-device-key", &mut okm)
            .expect("HKDF expand 32 bytes");
        okm
    }

    /// Derive message encryption key: HKDF(account_seed, timestamp) -> message_key
    pub fn derive_message_key(&self, timestamp: u64) -> [u8; 32] {
        let ts_bytes = timestamp.to_be_bytes();
        let hk = hkdf::Hkdf::<Sha256>::new(Some(&ts_bytes), &self.seed);
        let mut okm = [0u8; 32];
        hk.expand(b"laberry-msg-key", &mut okm)
            .expect("HKDF expand 32 bytes");
        okm
    }

    /// Derive static X25519 secret key from seed
    fn derive_secret_key(&self) -> SecretKey {
        let hk = hkdf::Hkdf::<Sha256>::new(Some(b"laberry-x25519"), &self.seed);
        let mut okm = [0u8; 32];
        hk.expand(b"x25519-secret", &mut okm)
            .expect("HKDF expand 32 bytes");
        SecretKey::from(okm)
    }

    /// Derive static X25519 public key from seed
    pub fn derive_public_key(&self) -> PublicKey {
        let secret = self.derive_secret_key();
        secret.public_key()
    }
}

// ==================== E2EE Message ====================

/// Ключ получателя
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2eeRecipientKey {
    pub ss: String,
    pub iv: String,
    pub ct: String,
}

/// Зашифрованное сообщение
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct E2eeMessage {
    pub v: u8,
    pub sender: i64,
    pub device_id: String,
    /// ephemeral public key (base64 X25519)
    pub epub: String,
    /// IV для шифрования (base64)
    pub iv: String,
    /// Зашифрованный payload (base64)
    pub payload: String,
    /// Ключи для получателей: user_id -> device_id -> E2eeRecipientKey
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keys: Option<HashMap<String, HashMap<String, E2eeRecipientKey>>>,
}

impl E2eeMessage {
    /// Зашифровать текст для получателей
    pub fn encrypt(
        sender_account: &AccountKey,
        sender_device_id: &str,
        sender_id: i64,
        plaintext: &str,
        recipients: &[(i64, &str, &AccountKey)],
    ) -> Result<Self, String> {
        // Генерируем ephemeral X25519 secret key
        let ep_secret = SecretKey::generate(&mut rand::thread_rng());
        let ep_pub = ep_secret.public_key();
        let epub_b64 = b64::encode(ep_pub.as_bytes());

        // Шифруем payload для каждого получателя
        let mut keys: HashMap<String, HashMap<String, E2eeRecipientKey>> = HashMap::new();
        for (recv_id, recv_device_id, recv_account) in recipients {
            let recv_pub = recv_account.derive_public_key();
            // DH: shared secret
            let ep_static = StaticSecret::from(ep_secret.to_bytes());
            let recv_pub_x = X25519PublicKey::from(recv_pub.as_bytes().clone());
            let shared = ep_static.diffie_hellman(&recv_pub_x).to_bytes();

            // HKDF для ключа шифрования получателя
            let salt = recv_device_id.as_bytes();
            let hk = hkdf::Hkdf::<Sha256>::new(Some(salt), &shared);
            let mut recv_key = [0u8; 32];
            hk.expand(b"laberry-recv-key", &mut recv_key)
                .map_err(|e| format!("HKDF error: {}", e))?;

            // IV для ChaCha20
            let iv: [u8; 12] = rand::random();

            // Шифруем ChaCha20-Poly1305
            let cipher = chacha20poly1305::ChaCha20Poly1305::new_from_slice(&recv_key)
                .map_err(|e| format!("ChaCha20Poly1305 new: {}", e))?;
            let nonce = chacha20poly1305::Nonce::from_slice(&iv);
            let ct = cipher
                .encrypt(&nonce, plaintext.as_bytes())
                .map_err(|e| format!("ChaCha20 encrypt: {}", e))?;

            let ct_b64 = b64::encode(&ct);
            let iv_b64 = b64::encode(&iv);

            keys.entry(recv_id.to_string())
                .or_default()
                .insert(recv_device_id.to_string(), E2eeRecipientKey {
                    ss: b64::encode(&shared),
                    iv: iv_b64,
                    ct: ct_b64,
                });
        }

        // Шифруем payload с помощью sender's account key (для себя)
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let sender_key = sender_account.derive_message_key(ts);
        let sender_iv: [u8; 12] = rand::random();
        let cipher = chacha20poly1305::ChaCha20Poly1305::new_from_slice(&sender_key)
            .map_err(|e| format!("ChaCha20Poly1305 new: {}", e))?;
        let nonce = chacha20poly1305::Nonce::from_slice(&sender_iv);
        let payload = cipher
            .encrypt(&nonce, plaintext.as_bytes())
            .map_err(|e| format!("ChaCha20 encrypt: {}", e))?;

        Ok(E2eeMessage {
            v: 1,
            sender: sender_id,
            device_id: sender_device_id.to_string(),
            epub: epub_b64,
            iv: b64::encode(&sender_iv),
            payload: b64::encode(&payload),
            keys: if keys.is_empty() { None } else { Some(keys) },
        })
    }

    /// Расшифровать сообщение
    pub fn decrypt(&self, my_account: &AccountKey, my_device_id: &str) -> Result<String, String> {
        if self.v != 1 {
            return Err(format!("Unsupported E2EE version: {}", self.v));
        }

        // Пытаемся найти свой ключ в keys
        if let Some(ref keys) = self.keys {
            // Ищем по sender -> device_id
            if let Some(device_keys) = keys.get(&self.sender.to_string()) {
                if let Some(rec_key) = device_keys.get(my_device_id) {
                    return Self::decrypt_with_shared(&self.epub, rec_key, my_account);
                }
            }
            // Ищем по любому user_id -> device_id
            for device_keys in keys.values() {
                if let Some(rec_key) = device_keys.get(my_device_id) {
                    return Self::decrypt_with_shared(&self.epub, rec_key, my_account);
                }
            }
        }

        // Если нет в keys — пробуем decrypt с account key (для себя)
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let msg_key = my_account.derive_message_key(ts);
        Self::decrypt_with_key(&self.iv, &self.payload, &msg_key)
    }

    fn decrypt_with_shared(
        epub_b64: &str,
        rec_key: &E2eeRecipientKey,
        my_account: &AccountKey,
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

        // Получаем наш статический secret key из account
        let my_secret = my_account.derive_secret_key();

        // DH: my_secret * ep_pub = shared
        let my_static = StaticSecret::from(my_secret.to_bytes());
        let ep_pub_x = X25519PublicKey::from(epub_bytes);
        let shared = my_static.diffie_hellman(&ep_pub_x).to_bytes();

        // shared уже как base key
        let salt = b"laberry-recv-key";
        let hk = hkdf::Hkdf::<Sha256>::new(Some(salt), &shared);
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

    fn decrypt_with_key(
        iv_b64: &str,
        payload_b64: &str,
        key: &[u8; 32],
    ) -> Result<String, String> {
        let iv_bytes = match b64::decode(iv_b64) {
            Some(b) => b,
            None => return Err("Invalid iv".to_string()),
        };
        let ct_bytes = match b64::decode(payload_b64) {
            Some(b) => b,
            None => return Err("Invalid payload".to_string()),
        };

        let cipher = chacha20poly1305::ChaCha20Poly1305::new_from_slice(key)
            .map_err(|e| format!("ChaCha20Poly1305 new: {}", e))?;
        let nonce = chacha20poly1305::Nonce::from_slice(&iv_bytes);

        let pt = cipher
            .decrypt(&nonce, ct_bytes.as_ref())
            .map_err(|e| format!("ChaCha20 decrypt: {}", e))?;

        String::from_utf8(pt).map_err(|e| format!("UTF-8: {}", e))
    }

    /// Check if content is an E2EE message
    pub fn is_e2ee_content(content: &str) -> bool {
        content.starts_with("[[e2ee:v1|")
    }

    /// Parse E2EE message from content string "[[e2ee:v1|<json>]]"
    pub fn from_content(content: &str) -> Option<Self> {
        if !Self::is_e2ee_content(content) {
            return None;
        }
        let inner = content.strip_prefix("[[e2ee:v1|")?;
        let json = inner.strip_suffix("]]")?;
        Self::from_json(json).ok()
    }

    /// Convert to content string "[[e2ee:v1|<json>]]"
    pub fn to_content(&self) -> String {
        if let Ok(json) = Self::to_json(self) {
            format!("[[e2ee:v1|{}]]", json)
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
    fn test_account_key_generate_and_serialize() {
        let key = AccountKey::generate();
        let b64 = key.to_base64();
        let restored = AccountKey::from_base64(&b64).unwrap();
        assert_eq!(key.seed, restored.seed);
    }

    #[test]
    fn test_account_key_fingerprint_deterministic() {
        let key = AccountKey::generate();
        let fp1 = key.fingerprint();
        let fp2 = key.fingerprint();
        assert_eq!(fp1, fp2);
        assert_eq!(fp1.len(), 64); // SHA-256 hex
    }

    #[test]
    fn test_encrypt_decrypt_same_account() {
        let sender = AccountKey::generate();
        let recipient = AccountKey::generate();

        let plaintext = "Hello, E2EE! 🔐";
        let device_id = "test-device-1";

        let msg = E2eeMessage::encrypt(
            &sender,
            device_id,
            42,
            plaintext,
            &[(999, device_id, &recipient)],
        ).unwrap();

        // Recipient decrypts
        let decrypted = msg.decrypt(&recipient, device_id);
        assert!(decrypted.is_ok(), "decrypt failed: {:?}", decrypted);
        assert_eq!(decrypted.unwrap(), plaintext);
    }

    #[test]
    fn test_encrypt_decrypt_self() {
        let key = AccountKey::generate();
        let plaintext = "Self-message";
        let device_id = "self-device";

        let msg = E2eeMessage::encrypt(
            &key,
            device_id,
            1,
            plaintext,
            &[(1, device_id, &key)],
        ).unwrap();

        let decrypted = msg.decrypt(&key, device_id);
        assert!(decrypted.is_ok());
        assert_eq!(decrypted.unwrap(), plaintext);
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
}
