# excalidraw-server

Self-hostable, optional real-time collaboration server for the desktop
Excalidraw app. It is a drop-in replacement for the Firebase backend the
official `excalidraw-app` uses for collaboration:

- **Socket.IO relay** (compatible with `socket.io-client@4.7.2`, the version
  vendored by the app) for the 8 collab events (`join-room`,
  `server-broadcast`, `server-volatile-broadcast`, `user-follow`,
  `init-room`, `new-user`, `room-user-change`, `client-broadcast`,
  `first-in-room`, `user-follow-room-change`).
- **REST API** for persisting the encrypted scene + file attachments,
  backed by SQLite and local disk instead of Firestore/Cloud Storage.

The server is protocol-blind: every scene/file/broadcast payload it stores
or relays is already AES-GCM encrypted client-side. It never sees plaintext,
and it doesn't need to -- it only implements room join/broadcast/presence
semantics and dumb encrypted-blob storage.

This server is entirely optional. If the desktop app has no server URL/token
configured, it makes zero network calls and behaves exactly as it does
without any of this.

## Running locally

Requires Rust (edition 2021 toolchain, stable) and no external services --
SQLite is embedded, and file attachments are stored on local disk.

```bash
cd server
INSTANCE_TOKEN=$(openssl rand -hex 32) cargo run
```

`INSTANCE_TOKEN` is the only required environment variable; the server
fails fast at startup with a clear error if it's missing or empty. On first
run it creates `./data/excalidraw.sqlite` (running migrations automatically)
and `./data/files/`.

For a release build (what Docker/CI use):

```bash
cargo build --release
INSTANCE_TOKEN=... ./target/release/excalidraw-server
```

## Configuration (environment variables)

| Variable          | Required | Default                     | Description |
|-------------------|----------|------------------------------|--------------|
| `INSTANCE_TOKEN`  | **yes**  | *(none -- fails fast if unset)* | Shared secret. Every REST request (except `/health`) needs `Authorization: Bearer <token>`, and every Socket.IO connection needs `auth: { token }` in the handshake. |
| `PORT`            | no       | `3002`                       | HTTP/WebSocket listen port. |
| `DATABASE_PATH`   | no       | `./data/excalidraw.sqlite`   | SQLite file path (created if missing; parent dir is created automatically). |
| `DATA_DIR`        | no       | `./data`                     | Root directory for file attachments (stored under `DATA_DIR/files/...`). |
| `ALLOWED_ORIGINS` | no       | *(empty)*                    | Comma-separated extra CORS origins, appended to the built-in `tauri://localhost` and `https://tauri.localhost`. Only needed if you also access the server from a browser. |
| `RUST_LOG`        | no       | `info`                       | Standard `tracing`/`env_logger`-style filter, e.g. `excalidraw_server=debug`. |

## REST API

All routes below except `/health` require `Authorization: Bearer <INSTANCE_TOKEN>`.

### `GET /health`

Liveness check, no auth required (used by the Docker `HEALTHCHECK` and for
reverse-proxy probes).

```bash
curl http://localhost:3002/health
```

### `PUT /scenes/:room_id`

Upserts the encrypted scene for a room. Body:

```json
{ "sceneVersion": 42, "iv": "<base64>", "ciphertext": "<base64>" }
```

```bash
curl -X PUT http://localhost:3002/scenes/my-room \
  -H "Authorization: Bearer $INSTANCE_TOKEN" \
  -H "Content-Type: application/json" \
  -d '{"sceneVersion": 1, "iv": "aXY=", "ciphertext": "Y2lwaGVy"}'
```

### `GET /scenes/:room_id`

Returns the stored scene, or `404` if the room has never been saved.

```bash
curl http://localhost:3002/scenes/my-room \
  -H "Authorization: Bearer $INSTANCE_TOKEN"
```

### `PUT /files/rooms/:room_id/:file_id`

Stores an encrypted file attachment for a collab room. Body is the raw
encrypted bytes (any `Content-Type`, e.g. `application/octet-stream`).

```bash
curl -X PUT http://localhost:3002/files/rooms/my-room/file-abc \
  -H "Authorization: Bearer $INSTANCE_TOKEN" \
  --data-binary @encrypted-blob.bin
```

### `GET /files/rooms/:room_id/:file_id`

Streams the file back with `Cache-Control: public, max-age=31536000`
(mirrors the original Firebase Storage cache behavior), or `404` if missing.

```bash
curl http://localhost:3002/files/rooms/my-room/file-abc \
  -H "Authorization: Bearer $INSTANCE_TOKEN" -o downloaded.bin
```

### `PUT /files/share-links/:share_id/:file_id` / `GET /files/share-links/:share_id/:file_id`

Same as the room file endpoints, but for files attached to a share-link
export rather than a live collab room.

```bash
curl -X PUT http://localhost:3002/files/share-links/my-share/file-xyz \
  -H "Authorization: Bearer $INSTANCE_TOKEN" \
  --data-binary @encrypted-blob.bin

curl http://localhost:3002/files/share-links/my-share/file-xyz \
  -H "Authorization: Bearer $INSTANCE_TOKEN" -o downloaded.bin
```

## Socket.IO protocol

Connect with the auth token in the handshake payload (this is what the
client is expected to send):

```js
io(serverUrl, { auth: { token: INSTANCE_TOKEN } });
```

A missing or wrong token causes the server to reject the handshake with a
`connect_error` (no `connect` event ever fires).

See `src/ws.rs` for the full event-by-event documentation, including a
detailed note on the `room-user-change` array-serialization pitfall in
socketioxide (a bare `Vec<String>` gets spread into N separate wire
arguments instead of sent as one array argument -- fixed by emitting a
1-tuple, `(vec,)`).

## Docker

The `Dockerfile` is a multi-stage build using `rust:1-alpine` for the build
stage. Alpine's own libc is musl, so building natively inside it already
produces a musl-linked binary without needing a musl cross-toolchain on the
host -- the final image is `alpine:3.20` plus a ~7 MB binary (~26 MB image
total).

```bash
docker build -t excalidraw-server .
docker run -d --name excalidraw-server \
  -p 3002:3002 \
  -e INSTANCE_TOKEN=$(openssl rand -hex 32) \
  -v excalidraw-data:/data \
  excalidraw-server
```

Or with Docker Compose (see `docker-compose.yml` for the full example with
comments):

```bash
export INSTANCE_TOKEN=$(openssl rand -hex 32)
docker compose up -d
```

Both were built and smoke-tested end-to-end (health check, scene PUT/GET)
as part of developing this server.

## Testing

Two independent test suites cover this server:

### REST integration tests (Rust)

`tests/rest_test.rs` spawns the real compiled binary (via
`CARGO_BIN_EXE_excalidraw-server`) on a random free port with an isolated
temp data dir, and exercises every REST route with `reqwest`: scene
PUT/GET round-trip, 404 on missing scene, binary file PUT/GET round-trip
(rooms and share-links), 401 on missing/wrong auth on every protected
route, and unauthenticated `/health`.

```bash
cargo test --release
```

### Socket.IO protocol tests (Node.js)

`tests/protocol_test.js` spawns the real compiled binary and drives it with
the **real, vendored `socket.io-client@4.7.2`** (from
`vendor/excalidraw/excalidraw-app/node_modules/socket.io-client` -- the
exact client version the desktop app uses), not a mock. It covers handshake
auth rejection (missing/wrong token), `first-in-room` for a lone client,
`new-user`/`room-user-change` when a second client joins (asserting
`room-user-change` arrives as a single array argument, not spread across N
arguments), `server-broadcast`/`server-volatile-broadcast` relay with real
binary payloads (verifying byte-for-byte fidelity and the 2-argument
`(encryptedData, iv)` shape), `room-user-change` on disconnect, and the full
`user-follow`/`user-follow-room-change` flow including disconnect-triggered
follower cleanup.

```bash
cargo build --release   # or `cargo build` for a debug binary
node tests/protocol_test.js
```

Exits non-zero if any assertion fails.

### Linting

```bash
cargo clippy --all-targets -- -D warnings
```

## Design notes / judgment calls

- **sqlx over rusqlite**: sqlx's async SQLite driver fits the existing
  tokio/axum/socketioxide async stack directly (no `spawn_blocking`
  boilerplate around a sync driver), and `sqlx::migrate!` gives versioned
  migrations essentially for free.
- **Migrations**: `sqlx::migrate!("./migrations")`, tracked in sqlx's own
  `_sqlx_migrations` table, run automatically at startup. Chosen over
  hand-rolled `CREATE TABLE IF NOT EXISTS` because it's barely more code and
  gives a real migration history for future schema changes.
- **musl via `rust:1-alpine` over cross-compiling from glibc**: no musl
  cross-toolchain was available on the dev host, and Alpine's native libc
  already *is* musl, so building inside an Alpine container sidesteps the
  cross-compilation question entirely while still producing a small,
  musl-linked binary.
- **`server-volatile-broadcast` is not actually volatile**: socketioxide
  0.14 has no fire-and-forget/volatile emit operator, so it's relayed
  through the same reliable path as `server-broadcast`. The spec only
  requires this to be "unreliable OK", not "must drop packets", so always
  delivering it is a safe superset of the required behavior.
- **No manual room-membership tracking**: rather than keeping a side
  `HashMap<room, Vec<socket_id>>` (as the earlier spike did, and which can
  drift from reality on disconnect races), every handler queries
  socketioxide's own adapter via `.sockets()`/`.rooms()`, which is the
  single source of truth for room membership.
- **`scene_version` is `i64` in the JSON body**, not `u64` as literally
  written in the original task description: SQLite integers are signed
  64-bit, and JS scene versions never come close to the signed range, so
  this avoids an unnecessary cast at the SQL boundary with no practical
  downside.
- **Path traversal guard**: `room_id`/`file_id`/`share_id` are validated to
  reject empty, `.`, `..`, and path separators before touching the
  filesystem, even though in practice they're opaque client-generated
  tokens.
- **CORS without credentials**: `Access-Control-Allow-Credentials` is not
  set, since auth is a Bearer token / handshake payload, not a cookie.
- **Single-node adapter only**: uses socketioxide's default in-memory
  `LocalAdapter` (no Redis/multi-node adapter). Fine for the intended
  self-hosted single-instance deployment; horizontal scaling is out of
  scope.

## Known gaps / follow-ups for review

- No request body size limit is set on the file upload routes beyond
  axum's defaults; consider adding `DefaultBodyLimit` matching the client's
  `FILE_UPLOAD_MAX_BYTES` (4 MiB) if abuse from a malicious/buggy client is
  a concern.
- No rate limiting / connection caps -- acceptable for a small self-hosted
  classroom-sized deployment, not for exposing directly to the open
  internet at scale.
- File records in the `files` table are never cleaned up (no TTL/GC for
  orphaned rooms), matching the "keep it simple" brief but worth revisiting
  if disk usage becomes a concern for long-lived instances.
- CORS origin list is static at startup (read once from `ALLOWED_ORIGINS`);
  changing it requires a restart.
