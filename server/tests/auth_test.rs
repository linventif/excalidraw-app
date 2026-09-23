//! Black-box integration tests for `AUTH_MODE=open-registration`: spawns the
//! real compiled server binary (same approach as `rest_test.rs`) configured
//! for open registration, and drives the real challenge/verify protocol
//! using the `crypto_box` crate as a stand-in NaCl-box client (the actual
//! wire-format fidelity against the real `tweetnacl` JS client is covered
//! separately by `tests/auth_protocol_test.js`; this file is about server
//! logic/state machine correctness: session issuance, username stability,
//! single-use/expiry, and rate limiting).

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine as _};
use crypto_box::{
    aead::{generic_array::GenericArray, Aead, OsRng},
    PublicKey, SalsaBox, SecretKey,
};
use serde_json::{json, Value};

struct TestServer {
    child: Child,
    base_url: String,
    _data_dir: tempfile::TempDir,
}

impl TestServer {
    /// Starts a server in `open-registration` mode. `extra_env` lets
    /// individual tests override things like `SESSION_TOKEN_TTL_SECONDS` or
    /// opt in to also setting `INSTANCE_TOKEN`.
    async fn start(extra_env: &[(&str, &str)]) -> Self {
        let port = pick_free_port();
        let data_dir = tempfile::tempdir().expect("tempdir");

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_excalidraw-server"));
        cmd.env("PORT", port.to_string())
            .env("AUTH_MODE", "open-registration")
            .env("DATA_DIR", data_dir.path())
            .env("DATABASE_PATH", data_dir.path().join("db.sqlite"))
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }

        let child = cmd.spawn().expect("failed to spawn excalidraw-server binary");

        let base_url = format!("http://127.0.0.1:{port}");
        let server = Self {
            child,
            base_url,
            _data_dir: data_dir,
        };
        server.wait_for_health().await;
        server
    }

    async fn wait_for_health(&self) {
        let client = reqwest::Client::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if let Ok(resp) = client.get(format!("{}/health", self.base_url)).send().await {
                if resp.status().is_success() {
                    return;
                }
            }
            if std::time::Instant::now() > deadline {
                panic!("server did not become healthy in time");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn pick_free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("bind ephemeral port")
        .local_addr()
        .expect("local_addr")
        .port()
}

// ---------------------------------------------------------------------------
// Minimal NaCl-box "client" (stands in for the real tweetnacl client; see
// module docs / auth_protocol_test.js for the real-client interop check)
// ---------------------------------------------------------------------------

struct Identity {
    secret: SecretKey,
    public_key_b64: String,
}

fn new_identity() -> Identity {
    let secret = SecretKey::generate(&mut OsRng);
    let public_key_b64 = STANDARD.encode(secret.public_key().as_bytes());
    Identity {
        secret,
        public_key_b64,
    }
}

/// Runs the full `/auth/challenge` -> decrypt -> `/auth/verify` dance and
/// returns the raw JSON verify response (caller decides success/failure).
async fn challenge_and_verify(
    client: &reqwest::Client,
    server: &TestServer,
    identity: &Identity,
) -> (reqwest::StatusCode, Value) {
    let challenge: Value = client
        .post(server.url("/auth/challenge"))
        .json(&json!({ "publicKey": identity.public_key_b64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let solution = decrypt_challenge(&challenge, identity);

    let resp = client
        .post(server.url("/auth/verify"))
        .json(&json!({
            "challengeId": challenge["challengeId"],
            "solution": STANDARD.encode(solution),
        }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let body: Value = resp.json().await.unwrap();
    (status, body)
}

/// Requests a challenge and decrypts it, returning `(challengeId, solution)`
/// without posting `/auth/verify` -- lets tests control exactly when/how
/// verify is called (e.g. to test replay).
async fn request_and_decrypt_challenge(
    client: &reqwest::Client,
    server: &TestServer,
    identity: &Identity,
) -> (String, Vec<u8>) {
    let challenge: Value = client
        .post(server.url("/auth/challenge"))
        .json(&json!({ "publicKey": identity.public_key_b64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let solution = decrypt_challenge(&challenge, identity);
    (
        challenge["challengeId"].as_str().unwrap().to_string(),
        solution,
    )
}

fn decrypt_challenge(challenge: &Value, identity: &Identity) -> Vec<u8> {
    let server_public_bytes: [u8; 32] = STANDARD
        .decode(challenge["serverPublicKey"].as_str().unwrap())
        .unwrap()
        .try_into()
        .unwrap();
    let server_public = PublicKey::from(server_public_bytes);
    let nonce_bytes = STANDARD.decode(challenge["nonce"].as_str().unwrap()).unwrap();
    let ciphertext = STANDARD
        .decode(challenge["encryptedChallenge"].as_str().unwrap())
        .unwrap();

    let salsa_box = SalsaBox::new(&server_public, &identity.secret);
    let nonce = GenericArray::from_slice(&nonce_bytes);
    salsa_box
        .decrypt(nonce, ciphertext.as_slice())
        .expect("decrypting the server's challenge should succeed")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn full_round_trip_issues_working_session_token() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();
    let identity = new_identity();

    let (status, body) = challenge_and_verify(&client, &server, &identity).await;
    assert_eq!(status, 200, "verify should succeed: {body:?}");
    assert!(body["username"].as_str().is_some_and(|u| !u.is_empty()));
    assert!(body["sessionToken"]
        .as_str()
        .is_some_and(|t| !t.is_empty()));
    assert!(body["expiresAt"].as_str().is_some_and(|t| !t.is_empty()));

    let session_token = body["sessionToken"].as_str().unwrap();

    // The session token must work like an instance token on a protected
    // route. 404 (not 401) proves auth passed and we reached the handler.
    let resp = client
        .get(server.url("/scenes/some-room-that-does-not-exist"))
        .bearer_auth(session_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "auth should pass; room legitimately doesn't exist");
}

#[tokio::test]
async fn same_public_key_registering_twice_returns_same_username() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();
    let identity = new_identity();

    let (status1, body1) = challenge_and_verify(&client, &server, &identity).await;
    assert_eq!(status1, 200);
    let username1 = body1["username"].as_str().unwrap().to_string();

    // Second registration from the SAME device (same keypair) must reuse the
    // existing account and return the identical username, with a distinct
    // session token (each verify issues its own fresh token).
    let (status2, body2) = challenge_and_verify(&client, &server, &identity).await;
    assert_eq!(status2, 200);
    let username2 = body2["username"].as_str().unwrap().to_string();

    assert_eq!(username1, username2, "re-authenticating device must keep its username");
    assert_ne!(
        body1["sessionToken"], body2["sessionToken"],
        "each verify should issue its own session token"
    );
}

#[tokio::test]
async fn different_public_keys_get_different_usernames() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();

    let (_, body_a) = challenge_and_verify(&client, &server, &new_identity()).await;
    let (_, body_b) = challenge_and_verify(&client, &server, &new_identity()).await;

    assert_ne!(body_a["username"], body_b["username"]);
}

#[tokio::test]
async fn wrong_solution_is_rejected() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();
    let identity = new_identity();

    let challenge: Value = client
        .post(server.url("/auth/challenge"))
        .json(&json!({ "publicKey": identity.public_key_b64 }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    let bogus_solution = STANDARD.encode([0u8; 32]);
    let resp = client
        .post(server.url("/auth/verify"))
        .json(&json!({
            "challengeId": challenge["challengeId"],
            "solution": bogus_solution,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn reused_challenge_id_is_rejected_on_second_use() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();
    let identity = new_identity();

    let (challenge_id, solution) = request_and_decrypt_challenge(&client, &server, &identity).await;
    let solution_b64 = STANDARD.encode(&solution);

    let first = client
        .post(server.url("/auth/verify"))
        .json(&json!({ "challengeId": challenge_id, "solution": solution_b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 200, "first use of a fresh challenge should succeed");

    // Replay with the exact same (challengeId, solution): must be rejected,
    // even though the solution is objectively correct -- single-use.
    let second = client
        .post(server.url("/auth/verify"))
        .json(&json!({ "challengeId": challenge_id, "solution": solution_b64 }))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), 401, "a challengeId must not be replayable");
}

#[tokio::test]
async fn unknown_challenge_id_is_rejected() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(server.url("/auth/verify"))
        .json(&json!({
            "challengeId": uuid::Uuid::new_v4().to_string(),
            "solution": STANDARD.encode([0u8; 32]),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn challenge_publickey_must_be_32_bytes() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();

    let resp = client
        .post(server.url("/auth/challenge"))
        .json(&json!({ "publicKey": STANDARD.encode([0u8; 16]) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn invalid_session_token_is_rejected_on_protected_route() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/scenes/some-room"))
        .bearer_auth("not-a-real-session-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn missing_auth_is_rejected_in_open_registration_mode_too() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();

    let resp = client.get(server.url("/scenes/some-room")).send().await.unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn expired_session_token_is_rejected() {
    // A 1s TTL (via the new SESSION_TOKEN_TTL_SECONDS env var) keeps this
    // test fast instead of waiting out the real default (24h).
    let server = TestServer::start(&[("SESSION_TOKEN_TTL_SECONDS", "1")]).await;
    let client = reqwest::Client::new();
    let identity = new_identity();

    let (status, body) = challenge_and_verify(&client, &server, &identity).await;
    assert_eq!(status, 200);
    let session_token = body["sessionToken"].as_str().unwrap().to_string();

    // Sanity check: works immediately.
    let resp = client
        .get(server.url("/scenes/some-room"))
        .bearer_auth(&session_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "should be valid immediately after issuance");

    tokio::time::sleep(Duration::from_millis(1500)).await;

    let resp = client
        .get(server.url("/scenes/some-room"))
        .bearer_auth(&session_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "should be rejected once past its TTL");
}

#[tokio::test]
async fn optional_instance_token_also_works_as_superuser_bearer() {
    // The operator can set INSTANCE_TOKEN even in open-registration mode, in
    // which case it must keep working as an additional valid bearer token
    // alongside per-user session tokens (not a requirement, just an option).
    let server = TestServer::start(&[("INSTANCE_TOKEN", "superuser-secret")]).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/scenes/some-room"))
        .bearer_auth("superuser-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404, "the optional instance token should still be accepted");

    let resp = client
        .get(server.url("/scenes/some-room"))
        .bearer_auth("wrong-secret")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn rate_limiting_kicks_in_under_rapid_repeated_attempts() {
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();

    let mut saw_429 = false;
    // Comfortably more than the server's per-minute-per-IP budget; all from
    // the same loopback client so they land in the same rate-limit bucket.
    for _ in 0..60 {
        let resp = client
            .post(server.url("/auth/challenge"))
            .json(&json!({ "publicKey": STANDARD.encode([1u8; 32]) }))
            .send()
            .await
            .unwrap();
        if resp.status() == 429 {
            saw_429 = true;
            break;
        }
    }
    assert!(saw_429, "expected rate limiting (429) after enough rapid attempts");
}

#[tokio::test]
async fn expired_challenge_is_rejected() {
    // Slow (~61s): intentionally exercises the real 60s challenge TTL rather
    // than mocking time, since that TTL isn't configurable (unlike the
    // session TTL) -- it's meant to be a short-lived, in-memory-only value
    // per the task brief, not something an operator should be tuning.
    let server = TestServer::start(&[]).await;
    let client = reqwest::Client::new();
    let identity = new_identity();

    let (challenge_id, solution) = request_and_decrypt_challenge(&client, &server, &identity).await;

    tokio::time::sleep(Duration::from_secs(61)).await;

    let resp = client
        .post(server.url("/auth/verify"))
        .json(&json!({
            "challengeId": challenge_id,
            "solution": STANDARD.encode(&solution),
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "a 61s-old challenge must be treated as expired even with the correct solution"
    );
}
