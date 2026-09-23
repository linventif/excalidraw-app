-- Backing tables for AUTH_MODE=open-registration (see src/open_auth.rs).
-- Each device generates its own X25519 keypair client-side and proves
-- possession of the matching private key via a short-lived, in-memory-only
-- challenge (never persisted -- see the ChallengeStore in src/open_auth.rs)
-- in exchange for a permanent, server-assigned username and a session token
-- that behaves like the manually-shared INSTANCE_TOKEN everywhere else.
--
-- These tables are created unconditionally regardless of AUTH_MODE (they
-- simply stay empty in instance-token mode), so switching AUTH_MODE later
-- never requires a manual schema migration step.

CREATE TABLE IF NOT EXISTS users (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    public_key  TEXT NOT NULL UNIQUE, -- base64 X25519 public key, the device's permanent identity
    username    TEXT NOT NULL UNIQUE, -- server-assigned, non-editable, e.g. "swift-otter"
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- Session tokens issued by POST /auth/verify. Deliberately in SQLite (unlike
-- the short-lived challenges) since these live much longer (default 24h,
-- see SESSION_TOKEN_TTL_SECONDS) and should survive a server restart.
CREATE TABLE IF NOT EXISTS sessions (
    token       TEXT PRIMARY KEY, -- opaque random session token, presented as a Bearer token
    public_key  TEXT NOT NULL REFERENCES users(public_key),
    expires_at  TEXT NOT NULL,
    created_at  TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_sessions_expires_at ON sessions (expires_at);
