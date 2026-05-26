# LaBerry Server

Real-time messaging server — a Discord-like chat platform with REST API + WebSocket, voice/WebRTC, file sharing, AI bot, and E2EE.

## Features

- **Real-time chat** via WebSocket with room-based subscriptions
- **REST API** — auth, users, servers, channels, DMs, messages, files, friends, presence
- **Voice/Video** — WebRTC signaling, STUN/TURN
- **File sharing** — upload/download with thumbnails, archive preview, Range support
- **AI bot** — Gemka III, OpenAI-compatible API (LM Studio / llama.cpp)
- **E2EE** — device key management (P-256 ECDH via JWK)
- **Admin panel** — local-only web UI for moderation
- **Admin CLI** — moderation commands (ban, delete, purge)
- **2FA** — email verification with rate limiting
- **Security** — Argon2 passwords, JWT (HS256), CSRF, GeoIP blocking, rate limiting, security headers (HSTS, CSP)

## Tech Stack

| Component | |
|---|---|
| **Language** | Rust 2021 edition |
| **Web framework** | Axum 0.7 |
| **Database** | SQLite via SQLx (WAL mode, 32-conn pool) |
| **Auth** | JWT (HS256) + Argon2 |
| **TLS** | rustls (native) or Caddy (reverse proxy) |
| **Async** | Tokio |

## Quick Start

### Prerequisites

- Rust 1.75+ (MSRV)
- SQLite

### Setup

```bash
cp .env.example .env
# Edit .env with your SECRET_KEY and settings
```

### Run

```bash
cd server
cargo run --release
```

### Environment Variables

See `.env.example` for all configuration options.

Key variables:
- `SECRET_KEY` — JWT signing key (min 64 chars)
- `DATABASE_URL` — SQLite path (default: `sqlite:./laberry.db`)
- `LB_HOST` / `LB_PORT` — bind address
- `LB_TLS_CERT_PATH` / `LB_TLS_KEY_PATH` — native TLS (optional)
- `CORS_ALLOWED_ORIGINS` — comma-separated origins

## Project Structure

```
server/src/
├── main.rs              # Entry point
├── lib.rs               # Module declarations
├── server.rs            # Axum app builder, router
├── auth.rs              # JWT + Argon2
├── db/                  # Database schema & migrations
├── middleware/           # Auth, CSRF, rate limit, GeoIP
├── routes/              # REST API handlers
├── ws/                  # WebSocket handlers (chat, presence)
├── ai_client.rs         # AI bot integration
├── admin_cli.rs         # Admin CLI tool
└── tls.rs               # TLS config
```

## API

Full API reference: [LaBerry-API.md](LaBerry-API.md)

### WebSocket (`/ws`)

- JWT auth via `?token=` query param or `Authorization` header
- Join/leave rooms, send messages, typing indicators
- Voice channel join/leave events
- RTC signaling relay

## Deployment Options

1. **Native TLS** — set `LB_TLS_CERT_PATH` / `LB_TLS_KEY_PATH`
2. **Behind Caddy** — included in `caddy/` directory, auto HTTPS
3. **TURN server** — Docker Compose in `turn/`

## License

MIT
