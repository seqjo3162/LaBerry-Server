# PostgreSQL Migration — Post-Audit Report

Status: **Stage 6 complete** — `cargo check --all-targets` ✅ and `cargo clippy --all-targets --all-features -- -D warnings` ✅ both clean (zero errors, zero warnings).

## What was migrated

All SQLite-only constructs → PostgreSQL across the Rust crate. The single `INSERT OR IGNORE` family and every `?` placeholder, SQLite datetime function, and `1/0` boolean idiom in SQL strings is gone.

### 1. Placeholders `?` → `$N`
Every `sqlx::query` / `query_scalar` / `query_as` placeholder converted to positional `$1…$N`, with the `.bind()` order **verified against each statement's column order** (no blind global regex — each conversion was reviewed against its column list).

Files handled this session (beyond the earlier batches):
- `routes/admin_panel/content.rs` — gif view/delete, app_downloads insert/update/delete, `expires_at <= NOW()` x2, `is_active` read as `bool`.
- `routes/twofa.rs` — backup-codes insert `VALUES($1,$2,$3)`.
- `routes/e2ee.rs` — participant check, room-key select (x2), room-key upsert `VALUES($1..$5)`.
- `routes/friends.rs` — friend_requests select.
- `routes/profile_files.rs` — 4 queries (`$1`).
- `routes/presence.rs` — `is_online = TRUE`.
- `routes/servers.rs` — the largest file (~40 statements): channel create/delete/merge, server create/join/list/discover, join-request list/accept/reject, invite, update, delete, remove-member.

### 2. SQLite constructs → PostgreSQL
- `INSERT OR IGNORE INTO … VALUES(?, ?)` (5 in servers.rs) → `INSERT INTO … VALUES($1, $2) ON CONFLICT (server_id, user_id) DO NOTHING` (constraint `uq_server_members` matches conflict target).
- `strftime('%Y-%m-%dT%H:%M:%SZ','now')` → `NOW()`.
- `substr(m.content, 1, 120)` left as-is — PostgreSQL supports the identical `substr(string, from, count)` signature (not converted, verified valid).
- leftover helper name `sqlite_now_iso` in `files/upload.rs` retained — its **body already uses PG** `to_char(now(), …)` and returns a valid TIMESTAMPTZ-compatible ISO string; only the misleading name remains (functionally correct, not SQLite).

### 3. Boolean `1/0` → `TRUE`/`FALSE` (schema-type verified)
All `is_*` columns are **BOOLEAN NOT NULL** in PG (`chats.is_private` default false, `servers.is_public` default true, `user_presence.is_online` default false, `users.is_banned`). Converted:
- `is_private = 0` → `is_private = FALSE`; INSERT literals `…, 0, …` → `…, FALSE, …`.
- `is_public` UPDATE/INSERT bind `if x {1} else {0}` → `.bind(is_public)` (bool) — was binding int to a bool column.
- `is_public = 1`/`COALESCE(is_public, 1)` removed → plain `s.is_public AS is_public` read as `bool` (col is NOT NULL).
- `is_banned = 0` → `is_banned = FALSE`.
- `COALESCE(p.is_online, 0) = 0` → `COALESCE(p.is_online, FALSE) = FALSE` (fixes a PG type-union error: bool + int).

### 4. Runtime boolean decodes (`row.get`)
The runtime decode was the critical class of bug (compiles but fails at decode if you read `is_*` as `i64`). Verified per read:
- **Read as `bool`** where the column is BOOLEAN and returned directly: `is_public` in servers list/discover/join-request views, `is_active` (app_downloads), `is_private` (chats).
- **Read as `i64` (correct — kept)** where the source is a **computed integer CASE** returning 0/1, not the raw column: `is_expired`/`is_active` in `files/*` (`CASE … THEN 1 ELSE 0 END`), `is_online` in friends/servers member lists (`CASE (…) THEN 0 ELSE 1 END`). These are intentionally integer.
- `ChatRow.is_private` (struct field, still `i64` for JSON API contract) now decoded from the bool column via `r.get::<bool,_>("is_private") as i64` — matches the pattern already used in `chats.rs`.

### 5. What was deliberately NOT changed
- `files/*` `is_expired`/`is_active` remain `i64` — they are integer CASE expressions, not bool columns.
- No `#[allow]` used to suppress anything.
- Table/column names, business logic, ordering of `.bind()`, and all security checks untouched.

## Verification
- `cargo check --all-targets` — clean (39s).
- `cargo clippy --all-targets --all-features -- -D warnings` — clean (12s).
- Final grep across `server/src` confirms **zero** `?` SQL placeholders, `INSERT OR`, `strftime`, `IFNULL`, `GROUP_CONCAT`, `last_insert_rowid`, `REPLACE INTO` remain. (All remaining `?` in code are Rust `?` operators or literal `"?"` strings.)
- `git diff --stat`: 57 files changed.

---

## Stage 7 — Runtime manual-check list

Compile-time is green, but SQL compatibility is validated only against a live PG. Un-`SELECT COALESCE(is_public, 1)` was removed, so the following paths must be smoke-tested:

1. **Create server** (`POST /servers`) → creates server + owner member (`ON CONFLICT DO NOTHING`) + default text/voice channels (`is_private = FALSE` insert). Verify owner sees `is_public: true` in `GET /servers`.
2. **Join public server** (`POST /servers/{id}/join`) — verify `is_public` bool read works (was an int-affect before).
3. **Create channel / delete channel** — duplicate-name check (`$1,$2,$3`), last-channel-of-kind guard, and the channel-merge `chat_reads`/`chat_participants` `ON CONFLICT` paths.
4. **Invite member** — `is_banned = FALSE` filter; `server_members` `ON CONFLICT DO NOTHING`.
5. **Join-request flow** — create/list incoming/list outgoing (`server_is_public` as bool), accept/reject (`decided_at/decided_by` `$`).
6. **GIF admin** — view/delete global GIF (`$1` + `scope='global'`).
7. **App-downloads admin** — upload (deactivate prior `is_active = FALSE`), list (`is_active: bool`), delete.
8. **Backup 2FA codes** — generate codes insert `VALUES($1,$2,$3)`.
9. **E2EE room keys** — save / get / get-all (`$1…$5`; `ON CONFLICT(user_id, chat_id)`).
10. **File expiry & orphan cleanup** — two `expires_at <= NOW()` queries in admin content cleanup.
11. **Members list / active-friends** — `is_online` integer-CASE decode (0/1 JSON), `COALESCE(p.is_online, FALSE)`.
12. **Profile-file avatar** — set/clear avatar (`avatar_file_id = $1`), delete orphan row.
13. **Presence** — `GET …/online` uses `is_online = TRUE`.