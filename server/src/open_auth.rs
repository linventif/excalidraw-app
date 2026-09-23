//! `AUTH_MODE=open-registration`: lets anyone create a permanent account on
//! a public instance without an operator-shared secret. The wire protocol
//! (exact field names/shapes) is fixed by the already-shipped client in
//! `excalidraw-app/data/identity.ts` -- this module is the server-side half.
//!
//! ## Protocol
//!
//! 1. The client generates an X25519 keypair once per device (`tweetnacl`'s
//!    `nacl.box.keyPair()`) and keeps it in `localStorage` forever.
//! 2. `POST /auth/challenge { publicKey }` -- the server generates a fresh,
//!    one-time X25519 keypair of its own, 32 random bytes as the challenge
//!    plaintext, and a random 24-byte nonce; encrypts the plaintext to the
//!    client's public key (NaCl box: X25519 + XSalsa20-Poly1305, via
//!    `crypto_box::SalsaBox` -- specifically the Salsa variant, not ChaCha,
//!    to stay wire-compatible with `tweetnacl`); and stashes
//!    `{ public_key, challenge_plaintext, expires_at }` in memory, keyed by a
//!    fresh `challenge_id`.
//! 3. The client decrypts with `nacl.box.open()` (proving it holds the
//!    private key matching the public key it registered) and echoes the
//!    plaintext back as `solution`.
//! 4. `POST /auth/verify { challengeId, solution }` -- the server looks up
//!    and immediately deletes the challenge (single-use), byte-compares
//!    `solution` to the stored plaintext, and on a match either looks up the
//!    existing user by public key (so the same device always gets the same
//!    username back) or creates one with a freshly generated username, then
//!    issues a session token.
//!
//! Note the server never needs to *decrypt* anything: only step 2
//! (encrypting the challenge) touches `crypto_box` at all. Verification is
//! just a byte comparison against what step 2 already generated and stored.
//!
//! ## Why in-memory for challenges, SQLite for sessions
//!
//! Challenges live at most 60 seconds and are single-use by design -- there
//! is no scenario where surviving a server restart is useful, so an
//! `Arc<Mutex<HashMap<..>>>` (swept lazily on insert/lookup) avoids both
//! schema churn for a table nothing ever needs to query historically and,
//! more importantly, avoids a DB write on every single challenge issuance
//! (this endpoint is unauthenticated and internet-facing, so keeping its hot
//! path off the DB is also a mild abuse-hardening measure). Sessions, by
//! contrast, default to a 24h TTL and should survive a restart (a user
//! shouldn't be silently logged out because the server redeployed), so they
//! go in the `sessions` SQLite table alongside `users`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine as _,
};
use crypto_box::{
    aead::{rand_core::RngCore, Aead, AeadCore, OsRng},
    PublicKey, SalsaBox, SecretKey,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::state::AppState;

/// How long a challenge may sit unanswered before it's treated as expired.
/// Short by design: this is a live round trip (client fetches, decrypts
/// in-process, and posts back), not something a human needs minutes for.
const CHALLENGE_TTL: Duration = Duration::from_secs(60);

/// Registers `/auth/challenge` and `/auth/verify`. Callers (see `main.rs`)
/// only merge this router into the app at all when `AUTH_MODE ==
/// open-registration`; in `instance-token` mode the routes simply don't
/// exist (a request to them 404s), which is simpler and more honest than a
/// route that exists but always answers "not enabled in this mode".
pub fn auth_router() -> Router<AppState> {
    Router::new()
        .route("/auth/challenge", post(post_challenge))
        .route("/auth/verify", post(post_verify))
}

// ---------------------------------------------------------------------------
// In-memory, single-use challenge store
// ---------------------------------------------------------------------------

struct ChallengeRecord {
    public_key: [u8; 32],
    challenge_plaintext: [u8; 32],
    expires_at: Instant,
}

/// `Arc<Mutex<HashMap<..>>>` rather than a background sweep task: with a 60s
/// TTL and lazy sweeping on both insert and lookup, the map is self-cleaning
/// under normal traffic without needing a scheduled task to own -- one less
/// thing to spawn, cancel-on-shutdown, or leak.
#[derive(Default)]
pub struct ChallengeStore {
    inner: Mutex<HashMap<Uuid, ChallengeRecord>>,
}

impl ChallengeStore {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn insert(&self, record: ChallengeRecord) -> Uuid {
        let id = Uuid::new_v4();
        let now = Instant::now();
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        // Opportunistic sweep so the map can't grow unbounded if callers
        // request many challenges and never complete the round trip.
        guard.retain(|_, r| r.expires_at > now);
        guard.insert(id, record);
        id
    }

    /// Single-use lookup: unconditionally removes the entry (found or not,
    /// expired or not) so a `challengeId` can never be replayed, per spec.
    /// Returns `None` for missing *or* expired entries -- callers must not
    /// distinguish the two in their response, to avoid leaking which case
    /// occurred.
    fn take(&self, id: &Uuid) -> Option<ChallengeRecord> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.remove(id) {
            Some(record) if record.expires_at > Instant::now() => Some(record),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// POST /auth/challenge
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeRequest {
    public_key: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ChallengeResponse {
    challenge_id: String,
    server_public_key: String,
    nonce: String,
    encrypted_challenge: String,
}

async fn post_challenge(
    State(state): State<AppState>,
    Json(body): Json<ChallengeRequest>,
) -> Response {
    let decoded = match STANDARD.decode(&body.public_key) {
        Ok(v) => v,
        Err(_) => return bad_request("publicKey must be valid base64"),
    };
    let client_public_bytes: [u8; 32] = match decoded.try_into() {
        Ok(a) => a,
        Err(_) => return bad_request("publicKey must decode to exactly 32 bytes"),
    };
    let client_public = PublicKey::from(client_public_bytes);

    // Fresh, one-time server keypair for this challenge only -- never
    // reused, never persisted.
    let server_secret = SecretKey::generate(&mut OsRng);
    let server_public = server_secret.public_key();

    let salsa_box = SalsaBox::new(&client_public, &server_secret);
    let nonce = SalsaBox::generate_nonce(&mut OsRng);

    let challenge_plaintext = random_bytes_32();

    let ciphertext = match salsa_box.encrypt(&nonce, challenge_plaintext.as_slice()) {
        Ok(c) => c,
        Err(err) => {
            tracing::error!(%err, "failed to encrypt auth challenge");
            return internal_error();
        }
    };

    let challenge_id = state.challenge_store.insert(ChallengeRecord {
        public_key: client_public_bytes,
        challenge_plaintext,
        expires_at: Instant::now() + CHALLENGE_TTL,
    });

    Json(ChallengeResponse {
        challenge_id: challenge_id.to_string(),
        server_public_key: STANDARD.encode(server_public.as_bytes()),
        nonce: STANDARD.encode(nonce.as_slice()),
        encrypted_challenge: STANDARD.encode(&ciphertext),
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// POST /auth/verify
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifyRequest {
    challenge_id: String,
    solution: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VerifyResponse {
    username: String,
    session_token: String,
    expires_at: String,
}

async fn post_verify(State(state): State<AppState>, Json(body): Json<VerifyRequest>) -> Response {
    let Ok(challenge_id) = Uuid::parse_str(&body.challenge_id) else {
        return invalid_or_expired();
    };
    let Some(record) = state.challenge_store.take(&challenge_id) else {
        return invalid_or_expired();
    };
    let Ok(solution) = STANDARD.decode(&body.solution) else {
        return invalid_or_expired();
    };

    if !constant_time_eq(&solution, &record.challenge_plaintext) {
        return invalid_or_expired();
    }

    let public_key_b64 = STANDARD.encode(record.public_key);

    let username = match get_or_create_user(&state.db, &public_key_b64).await {
        Ok(u) => u,
        Err(err) => {
            tracing::error!(%err, "failed to look up/create user during /auth/verify");
            return internal_error();
        }
    };

    let session = match issue_session(
        &state.db,
        &public_key_b64,
        state.config.session_token_ttl_seconds,
    )
    .await
    {
        Ok(s) => s,
        Err(err) => {
            tracing::error!(%err, "failed to issue session token during /auth/verify");
            return internal_error();
        }
    };

    tracing::info!(username, "issued session (open-registration)");

    Json(VerifyResponse {
        username,
        session_token: session.token,
        expires_at: session.expires_at,
    })
    .into_response()
}

/// Deliberately generic: whether the challenge was missing, expired, or the
/// solution was simply wrong, the client sees the same response, so none of
/// those cases can be distinguished from the outside.
fn invalid_or_expired() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({ "error": "invalid or expired challenge" })),
    )
        .into_response()
}

fn bad_request(msg: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(serde_json::json!({ "error": msg })),
    )
        .into_response()
}

fn internal_error() -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal error" })),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// Session issuance / validation
// ---------------------------------------------------------------------------

struct IssuedSession {
    token: String,
    expires_at: String,
}

async fn issue_session(
    db: &SqlitePool,
    public_key_b64: &str,
    ttl_seconds: i64,
) -> Result<IssuedSession, sqlx::Error> {
    let token = URL_SAFE_NO_PAD.encode(random_bytes_32());

    // Computed once in SQL (consistent with the rest of this codebase, which
    // uses SQLite's own `strftime` for timestamps rather than pulling in a
    // `time`/`chrono` crate) and reused for both the stored row and the
    // response body, so they can never disagree.
    let (expires_at,): (String,) = sqlx::query_as(
        "SELECT strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '+' || ?1 || ' seconds')",
    )
    .bind(ttl_seconds)
    .fetch_one(db)
    .await?;

    sqlx::query("INSERT INTO sessions (token, public_key, expires_at) VALUES (?1, ?2, ?3)")
        .bind(&token)
        .bind(public_key_b64)
        .bind(&expires_at)
        .execute(db)
        .await?;

    Ok(IssuedSession { token, expires_at })
}

/// Used by both the REST auth middleware (`auth.rs`) and the Socket.IO
/// handshake middleware (`ws.rs`) in `open-registration` mode.
pub async fn validate_session_token(db: &SqlitePool, token: &str) -> bool {
    let result = sqlx::query_as::<_, (i64,)>(
        "SELECT 1 FROM sessions WHERE token = ?1 AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
    )
    .bind(token)
    .fetch_optional(db)
    .await;

    match result {
        Ok(Some(_)) => true,
        Ok(None) => false,
        Err(err) => {
            tracing::error!(%err, "session token lookup failed");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// User lookup / creation + username generation
// ---------------------------------------------------------------------------

/// Bounded retry loop for the (vanishingly rare) case of a generated
/// username colliding with an existing one.
const MAX_USERNAME_ATTEMPTS: u32 = 20;

async fn get_or_create_user(
    db: &SqlitePool,
    public_key_b64: &str,
) -> Result<String, sqlx::Error> {
    if let Some(username) = lookup_username(db, public_key_b64).await? {
        return Ok(username);
    }

    for attempt in 0..MAX_USERNAME_ATTEMPTS {
        let candidate = generate_username(attempt);
        match sqlx::query("INSERT INTO users (public_key, username) VALUES (?1, ?2)")
            .bind(public_key_b64)
            .bind(&candidate)
            .execute(db)
            .await
        {
            Ok(_) => return Ok(candidate),
            Err(sqlx::Error::Database(db_err)) if db_err.is_unique_violation() => {
                // Either another request for this same public key won a
                // race against us (check first, in which case just use
                // whatever username it picked), or `candidate` itself
                // collided with someone else's username (in which case
                // retry with a freshly generated one).
                if let Some(username) = lookup_username(db, public_key_b64).await? {
                    return Ok(username);
                }
                continue;
            }
            Err(e) => return Err(e),
        }
    }

    // Practically unreachable (20 collisions in a row against a
    // multi-thousand-combination wordlist), but guarantee termination with
    // an outright-unique fallback rather than erroring out a legitimate
    // registration.
    let candidate = format!("user-{}", Uuid::new_v4().simple());
    sqlx::query("INSERT INTO users (public_key, username) VALUES (?1, ?2)")
        .bind(public_key_b64)
        .bind(&candidate)
        .execute(db)
        .await?;
    Ok(candidate)
}

async fn lookup_username(
    db: &SqlitePool,
    public_key_b64: &str,
) -> Result<Option<String>, sqlx::Error> {
    let row = sqlx::query_as::<_, (String,)>("SELECT username FROM users WHERE public_key = ?1")
        .bind(public_key_b64)
        .fetch_optional(db)
        .await?;
    Ok(row.map(|(username,)| username))
}

/// Docker-container-name-style "adjective-noun" usernames, e.g.
/// `swift-otter`. Wordlists are a few dozen entries each -- plenty for a
/// human-readable, forgettable-in-a-good-way handle; they don't need to
/// match any particular existing library. On any collision past the first
/// attempt, a short numeric suffix is appended to the next random pick to
/// further disambiguate.
const ADJECTIVES: &[&str] = &[
    "amber", "brave", "calm", "clever", "cosmic", "daring", "eager", "fuzzy", "gentle", "golden",
    "happy", "jolly", "keen", "lively", "lucky", "mellow", "misty", "nimble", "proud", "quiet",
    "quirky", "rapid", "silent", "sleepy", "sneaky", "solar", "spry", "stormy", "sunny", "swift",
    "vivid", "witty", "zesty", "bold", "crisp",
];

const NOUNS: &[&str] = &[
    "otter", "falcon", "panda", "tiger", "koala", "lynx", "heron", "badger", "dolphin", "gecko",
    "hawk", "ibis", "jaguar", "kite", "lemur", "mole", "newt", "owl", "puffin", "quokka", "raven",
    "seal", "toucan", "urchin", "viper", "walrus", "wombat", "yak", "zebra", "comet", "meteor",
    "nebula", "pebble", "willow", "cedar",
];

fn generate_username(attempt: u32) -> String {
    let adjective = ADJECTIVES[random_bounded(ADJECTIVES.len())];
    let noun = NOUNS[random_bounded(NOUNS.len())];
    if attempt == 0 {
        format!("{adjective}-{noun}")
    } else {
        // 0-9999: enough to make a repeat collision astronomically unlikely
        // without producing an ugly wall of digits.
        let suffix = random_bounded(10_000);
        format!("{adjective}-{noun}-{suffix}")
    }
}

fn random_bounded(bound: usize) -> usize {
    let mut rng = OsRng;
    (rng.next_u32() as usize) % bound
}

fn random_bytes_32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let mut rng = OsRng;
    rng.fill_bytes(&mut bytes);
    bytes
}

/// Manual constant-time comparison rather than pulling in `subtle` for one
/// call site: `solution`/`challenge_plaintext` are single-use 32-byte
/// secrets already protected by the challenge's 60s TTL and single-use
/// deletion, so a timing side channel here is not a realistic attack -- but
/// it costs nothing to close it off anyway for a value that travels over
/// the open internet.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
