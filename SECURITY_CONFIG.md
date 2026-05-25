# 🔐 LaBerry Security Configuration Guide

This guide covers maximum security setup for E2EE deployment with HTTPS.

## Prerequisites

- Domain: `laberry.ru` (SSL certificate required)
- SSL Certificate and Private Key files
- Reverse proxy (nginx/Caddy recommended) OR native TLS support

---

## ✅ Option 1: HTTPS via Reverse Proxy (RECOMMENDED)

Use Nginx or Caddy to handle TLS termination. This is the **recommended production setup**.

### Nginx Configuration Example

```nginx
server {
    listen 443 ssl http2;
    server_name laberry.ru;

    # SSL Certificate
    ssl_certificate /etc/letsencrypt/live/laberry.ru/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/laberry.ru/privkey.pem;

    # Strong SSL Configuration
    ssl_protocols TLSv1.3 TLSv1.2;
    ssl_ciphers HIGH:!aNULL:!MD5;
    ssl_prefer_server_ciphers on;
    ssl_session_cache shared:SSL:10m;
    ssl_session_timeout 10m;

    # Security Headers
    add_header Strict-Transport-Security "max-age=31536000; includeSubDomains; preload" always;
    add_header X-Content-Type-Options "nosniff" always;
    add_header X-Frame-Options "DENY" always;
    add_header X-XSS-Protection "1; mode=block" always;
    add_header Referrer-Policy "strict-origin-when-cross-origin" always;
    add_header Permissions-Policy "geolocation=(), microphone=(self), camera=()" always;

    # Proxy to Backend
    location / {
        proxy_pass http://127.0.0.1:5000;
        proxy_http_version 1.1;
        proxy_set_header Upgrade $http_upgrade;
        proxy_set_header Connection "upgrade";
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

### Docker Compose Example

```yaml
version: '3.8'

services:
  laberry:
    image: laberry/server:latest
    environment:
      LB_HOST: 127.0.0.1  # Only listen locally
      LB_PORT: 5000
      SECRET_KEY: ${SECRET_KEY}  # Min 32 bytes, cryptographically random
      DATABASE_URL: sqlite:/data/laberry.db
    volumes:
      - ./data:/data
    expose:
      - "5000"

  nginx:
    image: nginx:latest
    ports:
      - "80:80"
      - "443:443"
    volumes:
      - ./nginx.conf:/etc/nginx/conf.d/default.conf
      - /etc/letsencrypt:/etc/letsencrypt:ro  # SSL certs
    depends_on:
      - laberry
```

---

## ⚙️ Option 2: Native HTTPS (Alternative)

If not using a reverse proxy, configure native TLS:

### Environment Variables

```bash
# HTTPS/TLS Configuration
export LB_TLS_CERT_PATH="/path/to/certificate.pem"
export LB_TLS_KEY_PATH="/path/to/private.key"
export LB_HOST="0.0.0.0"        # Listen on all interfaces
export LB_PORT="443"             # HTTPS default

# Security
export SECRET_KEY="<min 64 random alphanumeric chars>"
export JWT_ISSUER="laberry"

# E2EE
export LB_DOMAIN="laberry.ru"

# Database
export DATABASE_URL="sqlite:/var/lib/laberry/laberry.db"
```

### Systemd Service Example

```ini
[Unit]
Description=LaBerry Server with HTTPS
After=network.target

[Service]
Type=simple
User=laberry
Environment="LB_TLS_CERT_PATH=/etc/laberry/cert.pem"
Environment="LB_TLS_KEY_PATH=/etc/laberry/key.pem"
Environment="LB_HOST=0.0.0.0"
Environment="LB_PORT=443"
Environment="SECRET_KEY=your_random_secret_here"
Environment="DATABASE_URL=sqlite:/var/lib/laberry/db.sqlite"
ExecStart=/usr/local/bin/laberry_server
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

---

## 🔑 E2EE Security Configuration

### Key Pinning

LaBerry automatically pins user device keys (SHA-256 fingerprints). If a key changes unexpectedly:

- Server detects the change → Returns `409 Conflict` with error `e2ee_public_key_changed`
- User must confirm the change in client settings
- Old key is replaced with new fingerprint

### Device Registration

```bash
# Register device key (via API)
curl -X POST https://laberry.ru/api/users/me/device-keys \
  -H "Authorization: Bearer <TOKEN>" \
  -H "Content-Type: application/json" \
  -d '{
    "device_id": "browser-uuid-or-random",
    "public_jwk": {
      "kty": "EC",
      "crv": "P-256",
      "x": "<base64url>",
      "y": "<base64url>"
    },
    "label": "Chrome on MacBook Pro"
  }'
```

Response includes `fingerprint` for manual verification.

---

## 🛡️ Two-Factor Authentication

### Setup 2FA

1. **Enable on user account**:
   ```bash
   POST /api/users/me/2fa/setup
   ```

2. **Generate backup codes** (stored encrypted in DB):
   ```bash
   GET /api/users/me/2fa/backup-codes
   ```

3. **Verify 2FA code during login**:
   ```bash
   POST /api/auth/verify-2fa
   Body: { "code": "123456" }
   ```

4. **Recovery via backup code**:
   ```bash
   POST /api/auth/verify-2fa
   Body: { "backup_code": "XXXX-XXXX-XXXX" }
   ```

---

## 🔄 Session Management

### Track Active Sessions

Each device now has its own session:

```bash
# List active sessions
GET /api/users/me/sessions

# Revoke specific session
DELETE /api/users/me/sessions/{session_id}
```

Session includes:
- Device ID
- Device label
- Last activity
- IP address (from X-Real-IP or X-Forwarded-For)
- User-Agent

---

## 🚨 Security Checklist

- [ ] **HTTPS Enabled**: All traffic encrypted (TLS 1.3+)
- [ ] **HSTS Header**: Present and set to 1 year minimum
- [ ] **Secret Key**: At least 64 random characters
- [ ] **SSL Certificate**: Valid, not self-signed, from trusted CA
- [ ] **CSP Header**: Configured for your domain
- [ ] **2FA Enabled**: For admin accounts and E2EE users
- [ ] **Backup Codes**: Generated and securely stored
- [ ] **Key Pinning**: Fingerprints stored server-side
- [ ] **Database**: Encrypted at rest (SQLite WAL mode enabled)
- [ ] **Rate Limiting**: Enabled on auth endpoints
- [ ] **Admin Panel**: HTTPS only, loopback-bound by default
- [ ] **Environment Variables**: Not in code, use `.env` or secrets manager

---

## 📊 Verification

### Check HTTPS Configuration

```bash
# Test HTTPS
curl -I https://laberry.ru

# Should show:
# - HTTP/2 or HTTP/1.1 with upgrade
# - Strict-Transport-Security header
# - X-Content-Type-Options: nosniff
```

### Verify E2EE Key Pinning

```bash
# Fetch device keys
curl https://laberry.ru/api/users/{user_id}/device-keys

# Response includes device fingerprints
```

### Test TLS Security

```bash
# Using testssl.sh
./testssl.sh https://laberry.ru

# Should show:
# - TLS 1.3 and 1.2 only
# - Strong ciphers
# - No vulnerabilities
```

---

## 🔧 Troubleshooting

### "e2ee_public_key_changed" Error

**Cause**: Device key was updated  
**Solution**: User confirms device change in settings

### TLS Certificate Not Loading

**Check**:
```bash
openssl x509 -in /path/to/cert.pem -text -noout
openssl pkey -in /path/to/key.pem -text -noout
```

### Session Tracking Issues

**Enable**: Ensure `user_sessions` table created (auto-migrated)  
**Check**:
```sql
SELECT COUNT(*) FROM user_sessions WHERE user_id = ?;
```

---

## 📚 References

- [OWASP HTTPS Best Practices](https://cheatsheetseries.owasp.org/cheatsheets/HTTPS_Cheat_Sheet.html)
- [Mozilla SSL Configuration Generator](https://ssl-config.mozilla.org/)
- [Let's Encrypt Free Certificates](https://letsencrypt.org/)
- [E2EE Key Pinning](https://en.wikipedia.org/wiki/Key_pinning)

---

## 🚀 Production Deployment

For maximum security in production:

1. **Use reverse proxy** (Nginx/Caddy) for TLS termination
2. **Enable HSTS preload**: Add to HSTS Preload List
3. **Monitor key changes**: Log unusual E2EE key updates
4. **Rotate secrets**: Change `SECRET_KEY` every 90 days
5. **Backup codes**: Regularly verify users have backup codes
6. **Security updates**: Monitor for Rust/Axum security advisories
7. **Rate limiting**: Adjust thresholds based on user patterns

---

**Last Updated**: 2026-05-25  
**Version**: 1.0.0
