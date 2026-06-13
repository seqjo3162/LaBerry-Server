// ======================================================
// 🔐 TLS/HTTPS Configuration for LaBerry Server
// ======================================================

use rustls_pemfile::{certs, private_key};
use std::fs;
use std::path::Path;

/// Load TLS certificate and private key from PEM files
/// 
/// # Arguments
/// * `cert_path` - Path to certificate file (can be full chain)
/// * `key_path` - Path to private key file
///
/// # Returns
/// (ServerConfig, or Error)
pub fn load_tls_config(
    cert_path: &str,
    key_path: &str,
) -> anyhow::Result<rustls::ServerConfig> {
    // Validate paths exist
    if !Path::new(cert_path).exists() {
        anyhow::bail!("Certificate file not found: {}", cert_path);
    }
    if !Path::new(key_path).exists() {
        anyhow::bail!("Private key file not found: {}", key_path);
    }

    // Read certificate chain
    let cert_file = fs::File::open(cert_path)?;
    let mut cert_reader = std::io::BufReader::new(cert_file);
    let cert_chain: Vec<rustls::pki_types::CertificateDer> = certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()?;

    if cert_chain.is_empty() {
        anyhow::bail!("No certificates found in {}", cert_path);
    }

    // Read private key
    let key_file = fs::File::open(key_path)?;
    let mut key_reader = std::io::BufReader::new(key_file);
    let key = private_key(&mut key_reader)?
        .ok_or_else(|| anyhow::anyhow!("No private keys found in {}", key_path))?;

    // Create server config
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;

    Ok(config)
}

/// Security headers middleware builder
pub struct SecurityHeaders;

impl SecurityHeaders {
    /// Generate security headers for HTTPS-only domain
    pub fn headers(domain: &str) -> Vec<(String, String)> {
        vec![
            (
                "Strict-Transport-Security".to_string(),
                "max-age=31536000; includeSubDomains; preload".to_string(),
            ),
            (
                "X-Content-Type-Options".to_string(),
                "nosniff".to_string(),
            ),
            (
                "X-Frame-Options".to_string(),
                "DENY".to_string(),
            ),
            (
                "X-XSS-Protection".to_string(),
                "1; mode=block".to_string(),
            ),
            (
                "Content-Security-Policy".to_string(),
                format!(
                    "default-src 'self' https://{}; \
                     script-src 'self' 'unsafe-inline'; \
                     style-src 'self' 'unsafe-inline'; \
                     img-src 'self' data: https:; \
                     font-src 'self' data:; \
                     connect-src 'self' wss://{} https://{}; \
                     media-src 'self'; \
                     object-src 'none'; \
                     frame-ancestors 'none'",
                    domain, domain, domain
                ),
            ),
            (
                "Referrer-Policy".to_string(),
                "strict-origin-when-cross-origin".to_string(),
            ),
            (
                "Permissions-Policy".to_string(),
                "geolocation=(), microphone=(), camera=(), payment=(), usb=()".to_string(),
            ),
            (
                "Cache-Control".to_string(),
                "no-cache, no-store, must-revalidate, private".to_string(),
            ),
            (
                "Pragma".to_string(),
                "no-cache".to_string(),
            ),
            (
                "Expires".to_string(),
                "0".to_string(),
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_security_headers() {
        let headers = SecurityHeaders::headers("laberry.ru");
        assert!(!headers.is_empty());
        
        let hsts = headers.iter().find(|(k, _)| k == "Strict-Transport-Security");
        assert!(hsts.is_some());
        assert!(hsts.unwrap().1.contains("31536000")); // 1 year
    }
}
