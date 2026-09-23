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
- Two auth modes, selected via `AUTH_MODE` (see "Auth modes" below):
  **`instance-token`** (default) for closed, invite-only deployments with a
  single operator-shared secret, and **`open-registration`** for a public
  instance where anyone can self-register a permanent identity via a
  public-key challenge/response, no shared secret required.

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

In the default `instance-token` mode shown above, `INSTANCE_TOKEN` is the
only required environment variable; the server fails fast at startup with a
clear error if it's missing or empty. On first run it creates
`./data/excalidraw.sqlite` (running migrations automatically) and
`./data/files/`. For a public, open-registration instance instead, see
"Auth modes" below.

For a release build (what Docker/CI use):

```bash
cargo build --release
INSTANCE_TOKEN=... ./target/release/excalidraw-server
```

## Configuration (environment variables)

| Variable                     | Required | Default                     | Description |
|------------------------------|----------|------------------------------|--------------|
| `AUTH_MODE`                  | no       | `instance-token`             | `instance-token` (default) or `open-registration`. See "Auth modes" below. |
| `INSTANCE_TOKEN`             | mode-dependent | *(none -- fails fast if unset in `instance-token` mode)* | In `instance-token` mode: **required** shared secret, exactly as before. In `open-registration` mode: **optional** -- if set, it's accepted as an additional superuser bearer token alongside per-user session tokens, but nothing requires it. |
| `SESSION_TOKEN_TTL_SECONDS`  | no       | `86400` (24h)                | `open-registration` mode only: how long a session token issued by `POST /auth/verify` stays valid. Ignored in `instance-token` mode. |
| `PORT`                       | no       | `3002`                       | HTTP/WebSocket listen port. |
| `DATABASE_PATH`              | no       | `./data/excalidraw.sqlite`   | SQLite file path (created if missing; parent dir is created automatically). |
| `DATA_DIR`                   | no       | `./data`                     | Root directory for file attachments (stored under `DATA_DIR/files/...`). |
| `ALLOWED_ORIGINS`            | no       | *(empty)*                    | Comma-separated extra CORS origins, appended to the built-in `tauri://localhost` and `https://tauri.localhost`. Only needed if you also access the server from a browser. |
| `RUST_LOG`                   | no       | `info`                       | Standard `tracing`/`env_logger`-style filter, e.g. `excalidraw_server=debug`. |

## Auth modes

### `instance-token` (default)

Unchanged from the original design: a single operator-shared secret gates
every REST route (except `/health`) and every Socket.IO connection. Meant
for closed, invite-only deployments (e.g. a classroom) where the operator
hands the same `INSTANCE_TOKEN` to every trusted user out of band.

```bash
INSTANCE_TOKEN=$(openssl rand -hex 32) cargo run
```

### `open-registration`

For a public instance (e.g. `excalidraw.linv.dev`) where anyone can create
an account with no shared secret. Each client generates its own X25519
keypair locally (never sent to the server -- only the public half is),
proves it holds the matching private key via an encrypted challenge, and
gets back a permanent, server-assigned, non-editable username (Docker/GitHub
-style "adjective-animal", e.g. `swift-otter`) plus a session token used
exactly like `INSTANCE_TOKEN` everywhere else (`Authorization: Bearer
<sessionToken>` on REST, `auth: { token: sessionToken }` on the Socket.IO
handshake).

```bash
AUTH_MODE=open-registration cargo run
# INSTANCE_TOKEN is optional here -- set it too if you also want a
# superuser secret that bypasses per-user accounts entirely.
```

`/auth/challenge` and `/auth/verify` (see below) only exist in this mode --
in `instance-token` mode, requests to them 404, since the route is never
mounted at all rather than existing-but-disabled.

This surface is unauthenticated by design (it *is* the auth flow) and meant
to be reachable from the open internet, so both routes are rate-limited
per-IP (a simple in-memory sliding window, a couple dozen attempts per
minute) -- see "Design notes" below.

The crypto is a NaCl "box": X25519 ECDH + XSalsa20-Poly1305 AEAD, exactly
what JS's `tweetnacl` (`nacl.box`/`nacl.box.open`) speaks, implemented
server-side with the `crypto_box` crate's `SalsaBox` (not `ChaChaBox` --
`SalsaBox` specifically is required for XSalsa20 wire compatibility with
`tweetnacl`). All binary values (keys, nonces, ciphertexts) are standard
(non-URL-safe) base64 strings over the wire.

This flow is normally driven entirely by the app's own key exchange -- see
`excalidraw-app/data/identity.ts` for the real client implementation
(`getOrCreateIdentity`, `registerOrLogin`). Hand-rolling a full curl+openssl
NaCl-box example isn't practical (there's no standard CLI NaCl-box tool), so
the examples below only show the request/response *shapes*; treat
`identity.ts` as the authoritative client.

#### `POST /auth/challenge`

No auth required. Request:

```bash
curl -X POST http://localhost:3002/auth/challenge \
  -H "Content-Type: application/json" \
  -d '{"publicKey": "<base64, 32-byte X25519 public key>"}'
```

Response:

```json
{
  "challengeId": "<uuid>",
  "serverPublicKey": "<base64, 32 bytes>",
  "nonce": "<base64, 24 bytes>",
  "encryptedChallenge": "<base64, ciphertext+tag>"
}
```

The client decrypts `encryptedChallenge` with `nacl.box.open(encryptedChallenge, nonce, serverPublicKey, mySecretKey)`.
The challenge is single-use and expires after 60 seconds.

#### `POST /auth/verify`

No auth required. Request:

```bash
curl -X POST http://localhost:3002/auth/verify \
  -H "Content-Type: application/json" \
  -d '{"challengeId": "<from above>", "solution": "<base64, the decrypted challenge plaintext>"}'
```

Response:

```json
{ "username": "swift-otter", "sessionToken": "<opaque token>", "expiresAt": "2026-01-01T00:00:00.000Z" }
```

`sessionToken` is then used exactly like `INSTANCE_TOKEN` on every other
route. The same public key always gets the same username back on
re-authentication. A wrong solution, or a missing/expired/already-used
`challengeId`, all get the same generic `401 { "error": "invalid or expired challenge" }`
-- the three cases are deliberately indistinguishable from the outside.

## REST API

All routes below except `/health` (and, in `open-registration` mode,
`/auth/challenge` / `/auth/verify`) require `Authorization: Bearer <token>`
-- `INSTANCE_TOKEN` in `instance-token` mode, a session token (or the
optional `INSTANCE_TOKEN`) in `open-registration` mode. The examples below
use `$INSTANCE_TOKEN`, but any accepted token works the same way.

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
client is expected to send). In `instance-token` mode this is
`INSTANCE_TOKEN`; in `open-registration` mode it's the `sessionToken` from
`POST /auth/verify` (or the optional `INSTANCE_TOKEN`, if the operator set
one):

```js
io(serverUrl, { auth: { token } });
```

A missing or invalid/expired token causes the server to reject the
handshake with a `connect_error` (no `connect` event ever fires).

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

Four independent test suites cover this server:

### REST integration tests (Rust)

`tests/rest_test.rs` spawns the real compiled binary (via
`CARGO_BIN_EXE_excalidraw-server`) on a random free port with an isolated
temp data dir, and exercises every REST route with `reqwest`: scene
PUT/GET round-trip, 404 on missing scene, binary file PUT/GET round-trip
(rooms and share-links), 401 on missing/wrong auth on every protected
route, and unauthenticated `/health`. Runs in the default `instance-token`
mode -- this is the regression suite proving that mode is unaffected by
`open-registration`.

`tests/auth_test.rs` is the same black-box approach, but spawns the binary
with `AUTH_MODE=open-registration` and drives the challenge/verify protocol
using the `crypto_box` crate as a stand-in client (see `auth_protocol_test.js`
below for the real-`tweetnacl` version). Covers: a full challenge -> verify
round trip issuing a working session token that's then accepted on a
protected route; the same public key re-registering getting back the exact
same username; different public keys getting different usernames; a wrong
solution being rejected; a `challengeId` being rejected on reuse (replay)
and once genuinely past its 60s TTL; an unknown `challengeId` being
rejected; a malformed (wrong-length) `publicKey` being rejected with `400`;
an invalid or expired session token being rejected (a short
`SESSION_TOKEN_TTL_SECONDS` is used to make the expiry case fast rather than
waiting out the real default); the optional `INSTANCE_TOKEN` still working
as a superuser bearer token in this mode; and rate limiting eventually
kicking in under rapid repeated attempts.

```bash
cargo test --release
```

Note: `expired_challenge_is_rejected` genuinely sleeps ~61 real seconds (the
60s challenge TTL isn't configurable -- it's meant to be a short, fixed,
in-memory-only value, not an operator-tunable knob), so the full `cargo
test` run takes just over a minute.

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
follower cleanup. Runs against `instance-token` mode.

```bash
cargo build --release   # or `cargo build` for a debug binary
node tests/protocol_test.js
```

### Auth protocol interop test (Node.js, real `tweetnacl`)

`tests/auth_protocol_test.js` is the critical wire-format fidelity check for
`open-registration` mode: it spawns the real compiled binary and drives the
**full real flow with the actual `tweetnacl`/`tweetnacl-util` libraries**
(the same ones `excalidraw-app/data/identity.ts` uses), not the Rust
`crypto_box` crate acting as its own client. It generates a real
`nacl.box.keyPair()`, calls `POST /auth/challenge`, decrypts the response
with `nacl.box.open()` (this is the assertion that actually matters: it
proves the Rust server's `crypto_box::SalsaBox` output is byte-for-byte
consumable by real `tweetnacl`), calls `POST /auth/verify`, and then uses
the returned session token against a protected REST route. It also
re-authenticates the same identity a second time and checks the username
comes back unchanged. This is exactly the kind of check that previously
caught the `room-user-change` array-serialization bug -- a Rust-only test
can silently agree with itself about a wire format that a real JS client
doesn't actually speak.

```bash
cargo build --release
node tests/auth_protocol_test.js
```

Both Node test files resolve their real dependencies from
`vendor/excalidraw`'s installed `node_modules` rather than vendoring their
own copies, so `yarn install` must have been run there at least once.
`tweetnacl`/`tweetnacl-util` happen to be hoisted by yarn workspaces to
`vendor/excalidraw/node_modules/` (the workspace root) rather than
`vendor/excalidraw/excalidraw-app/node_modules/` (where `socket.io-client`
lives) -- `auth_protocol_test.js` checks both locations.

Exits non-zero if any assertion fails, in all Node.js test files.

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
- **In-memory challenge store, SQLite session store**: challenges live at
  most 60s and are single-use, so there's no scenario where surviving a
  restart is useful -- an `Arc<Mutex<HashMap<Uuid, _>>>` (swept lazily on
  insert/lookup) avoids both schema churn and a DB write on every single
  challenge issuance, which matters more than usual here since
  `/auth/challenge` is unauthenticated and internet-facing. Sessions default
  to a 24h TTL and should survive a restart, so they're a real SQLite table.
- **`crypto_box::SalsaBox`, not `ChaChaBox`**: `crypto_box` supports both
  NaCl-box variants, but only `SalsaBox` (XSalsa20-Poly1305) is wire-compatible
  with `tweetnacl`'s `nacl.box`, which is a hard requirement since the
  client side is fixed. Verified against the real `tweetnacl` library in
  `tests/auth_protocol_test.js`, not just asserted in a comment.
- **The server never decrypts anything in this protocol**: `/auth/challenge`
  encrypts a random plaintext to the client; `/auth/verify` just
  byte-compares the client's decrypted `solution` against what was already
  stored. No decryption ever happens server-side, which is a nice property
  for an unauthenticated, internet-facing endpoint -- less crypto code on
  the hot path, not more.
- **Hand-rolled per-IP rate limiter over `governor`/`tower_governor`**: the
  requirement ("a handful of attempts per minute per IP") is a couple dozen
  lines of `HashMap<IpAddr, Vec<Instant>>` bookkeeping (`src/rate_limit.rs`);
  a full token-bucket crate (plus its `dashmap`/`quanta` transitive deps)
  seemed like a lot of new dependency surface for a requirement this narrow,
  especially given this codebase's existing preference for small hand-rolled
  primitives over new crates where the need is contained.
- **Rate limiter keys on `X-Forwarded-For` when present, else the raw TCP
  peer address**: this repo already deploys behind a reverse proxy
  (Dokploy), so trusting the raw peer address unconditionally would rate-limit
  the proxy, not real clients. `X-Forwarded-For` is trusted without
  validation, which is fine for a rate *limit* (spoofing it can only hurt
  the spoofer's own budget) but would not be an appropriate trust model for
  anything security-critical like an IP allowlist.
- **`/auth/challenge` and `/auth/verify` are only mounted at all in
  `open-registration` mode**, rather than always-mounted-but-400ing in
  `instance-token` mode: a route that doesn't exist (404) seemed more honest
  than one that exists purely to reject every request, and it means the two
  modes can never accidentally share auth-route state.
- **Manual constant-time comparison for the challenge solution**, rather
  than pulling in `subtle` (which is already a transitive dependency via
  `curve25519-dalek`) for one call site: `solution` and
  `challenge_plaintext` are single-use 32-byte secrets already protected by
  the challenge's 60s TTL and single-use deletion, so a timing side channel
  here isn't a realistic attack, but closing it off costs nothing.
- **Usernames are generated in-process (no external wordlist crate)**: two
  hand-picked ~35-word lists (`ADJECTIVES`/`NOUNS` in `src/open_auth.rs`)
  combine to well over 1,000 unique pairs before any numeric suffix is
  needed, which is plenty for a "fun, memorable" handle; collisions are
  handled by regenerating (not just incrementing a suffix) up to 20 times,
  then falling back to a uuid-suffixed name that's guaranteed unique.

## Known gaps / follow-ups for review

- No request body size limit is set on the file upload routes beyond
  axum's defaults; consider adding `DefaultBodyLimit` matching the client's
  `FILE_UPLOAD_MAX_BYTES` (4 MiB) if abuse from a malicious/buggy client is
  a concern.
- Connection-level rate limiting/caps for the REST/Socket.IO routes
  themselves (as opposed to the two `open-registration` auth routes, which
  now are rate-limited) are still out of scope -- fine for a small
  self-hosted classroom-sized deployment, worth revisiting if
  `instance-token` mode is ever exposed directly to the open internet too.
- The `/auth/challenge` + `/auth/verify` rate limiter is in-memory and
  per-process: it resets on restart and doesn't share state across multiple
  server instances behind a load balancer. Fine for the intended
  single-instance deployment (see "single-node adapter only" above); would
  need a shared store (e.g. Redis) to hold under horizontal scaling.
- No account deletion / GDPR-style data export for `open-registration`
  users -- a public key + username + timestamp is not very sensitive, but
  this wasn't asked for and hasn't been built.
- File records in the `files` table are never cleaned up (no TTL/GC for
  orphaned rooms), matching the "keep it simple" brief but worth revisiting
  if disk usage becomes a concern for long-lived instances.
- CORS origin list is static at startup (read once from `ALLOWED_ORIGINS`);
  changing it requires a restart.
