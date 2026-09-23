//! Black-box REST integration tests: spawn the real compiled server binary
//! (via `CARGO_BIN_EXE_excalidraw-server`, the standard cargo mechanism for
//! integration tests to find a sibling `[[bin]]` target) on a random free
//! port with an isolated data dir, and exercise it over HTTP with `reqwest`.
//!
//! This is a black-box approach rather than calling into the crate as a
//! library, since `excalidraw-server` is a plain binary crate (no `lib.rs`)
//! -- spawning the real binary is simpler than restructuring the crate, and
//! it also exercises the exact startup path (env parsing, migrations,
//! listener binding) that production actually runs.

use std::net::TcpListener;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

struct TestServer {
    child: Child,
    base_url: String,
    token: String,
    _data_dir: tempfile::TempDir,
}

impl TestServer {
    async fn start() -> Self {
        let port = pick_free_port();
        let data_dir = tempfile::tempdir().expect("tempdir");
        let token = format!("test-token-{port}");

        let child = Command::new(env!("CARGO_BIN_EXE_excalidraw-server"))
            .env("PORT", port.to_string())
            .env("INSTANCE_TOKEN", &token)
            .env("DATA_DIR", data_dir.path())
            .env("DATABASE_PATH", data_dir.path().join("db.sqlite"))
            .env("RUST_LOG", "error")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn excalidraw-server binary");

        let base_url = format!("http://127.0.0.1:{port}");
        let server = Self {
            child,
            base_url,
            token,
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

#[tokio::test]
async fn health_ok_without_auth() {
    let server = TestServer::start().await;
    let resp = reqwest::get(server.url("/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[tokio::test]
async fn scene_put_then_get_roundtrips() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let body = serde_json::json!({
        "sceneVersion": 42,
        "iv": "aXZiYXNlNjQ=",
        "ciphertext": "Y2lwaGVydGV4dGJhc2U2NA==",
    });

    let put_resp = client
        .put(server.url("/scenes/room-abc"))
        .bearer_auth(&server.token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 204, "PUT scene should succeed");

    let get_resp = client
        .get(server.url("/scenes/room-abc"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200, "GET scene should succeed after PUT");

    let returned: serde_json::Value = get_resp.json().await.unwrap();
    assert_eq!(returned["sceneVersion"], 42);
    assert_eq!(returned["iv"], "aXZiYXNlNjQ=");
    assert_eq!(returned["ciphertext"], "Y2lwaGVydGV4dGJhc2U2NA==");

    // A second PUT (same room) should overwrite, not conflict.
    let body2 = serde_json::json!({
        "sceneVersion": 43,
        "iv": "aXZiYXNlNjQ=",
        "ciphertext": "dXBkYXRlZC1jaXBoZXJ0ZXh0",
    });
    let put_resp2 = client
        .put(server.url("/scenes/room-abc"))
        .bearer_auth(&server.token)
        .json(&body2)
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp2.status(), 204);

    let get_resp2 = client
        .get(server.url("/scenes/room-abc"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    let returned2: serde_json::Value = get_resp2.json().await.unwrap();
    assert_eq!(returned2["sceneVersion"], 43);
    assert_eq!(returned2["ciphertext"], "dXBkYXRlZC1jaXBoZXJ0ZXh0");
}

#[tokio::test]
async fn scene_get_missing_room_returns_404() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/scenes/does-not-exist"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn file_put_then_get_roundtrips_binary_content_for_rooms() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let payload: Vec<u8> = (0..=255u8).collect(); // exercise all byte values, incl. NUL

    let put_resp = client
        .put(server.url("/files/rooms/room-1/file-1"))
        .bearer_auth(&server.token)
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 204);

    let get_resp = client
        .get(server.url("/files/rooms/room-1/file-1"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    assert_eq!(
        get_resp
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("public, max-age=31536000")
    );

    let received = get_resp.bytes().await.unwrap();
    assert_eq!(received.as_ref(), payload.as_slice());
}

#[tokio::test]
async fn file_put_then_get_roundtrips_binary_content_for_share_links() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let payload = b"share-link-encrypted-blob".to_vec();

    let put_resp = client
        .put(server.url("/files/share-links/share-1/file-9"))
        .bearer_auth(&server.token)
        .body(payload.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(put_resp.status(), 204);

    let get_resp = client
        .get(server.url("/files/share-links/share-1/file-9"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(get_resp.status(), 200);
    let received = get_resp.bytes().await.unwrap();
    assert_eq!(received.as_ref(), payload.as_slice());
}

#[tokio::test]
async fn file_get_missing_returns_404() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/files/rooms/nope/nope"))
        .bearer_auth(&server.token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test]
async fn protected_routes_reject_missing_auth() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let cases: Vec<(reqwest::Method, String)> = vec![
        (reqwest::Method::GET, server.url("/scenes/room-x")),
        (reqwest::Method::PUT, server.url("/scenes/room-x")),
        (reqwest::Method::GET, server.url("/files/rooms/room-x/file-x")),
        (reqwest::Method::PUT, server.url("/files/rooms/room-x/file-x")),
        (
            reqwest::Method::GET,
            server.url("/files/share-links/s/file-x"),
        ),
        (
            reqwest::Method::PUT,
            server.url("/files/share-links/s/file-x"),
        ),
    ];

    for (method, url) in cases {
        let resp = client
            .request(method.clone(), &url)
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            401,
            "{method} {url} should be rejected without an Authorization header"
        );
    }
}

#[tokio::test]
async fn protected_routes_reject_wrong_auth() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/scenes/room-x"))
        .bearer_auth("wrong-token")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);

    let resp = client
        .put(server.url("/scenes/room-x"))
        .bearer_auth("wrong-token")
        .json(&serde_json::json!({"sceneVersion": 1, "iv": "aQ==", "ciphertext": "Yw=="}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}

#[tokio::test]
async fn health_does_not_require_auth_even_with_garbage_header() {
    let server = TestServer::start().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(server.url("/health"))
        .header("Authorization", "Bearer nonsense")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
}
