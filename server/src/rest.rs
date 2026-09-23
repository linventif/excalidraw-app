//! REST persistence API, replacing Firebase Firestore (scenes) and Cloud
//! Storage (files) with SQLite + local disk. The server never sees
//! plaintext: `iv`/`ciphertext` are opaque base64 strings produced
//! client-side, and file bodies are opaque encrypted byte blobs.

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, put},
    Json, Router,
};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::state::AppState;

/// Routes that require `Authorization: Bearer <INSTANCE_TOKEN>` (everything
/// except `/health`). The auth middleware is attached by the caller via
/// `route_layer` so it's easy to see, at the call site in `main.rs`, exactly
/// which routes are protected.
pub fn protected_router() -> Router<AppState> {
    Router::new()
        .route("/scenes/:room_id", put(put_scene).get(get_scene))
        .route(
            "/files/rooms/:room_id/:file_id",
            put(put_room_file).get(get_room_file),
        )
        .route(
            "/files/share-links/:share_id/:file_id",
            put(put_share_file).get(get_share_file),
        )
}

pub fn health_router() -> Router<AppState> {
    Router::new().route("/health", get(health))
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Only room ids / file ids that can't escape their directory are accepted.
/// These are normally opaque random tokens generated client-side, so this
/// should never reject legitimate traffic -- it's a defense-in-depth check
/// against path traversal.
fn is_safe_segment(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && !s.contains('/') && !s.contains('\\')
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScenePut {
    scene_version: i64,
    iv: String,
    ciphertext: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SceneResponse {
    scene_version: i64,
    iv: String,
    ciphertext: String,
}

async fn put_scene(
    State(state): State<AppState>,
    Path(room_id): Path<String>,
    Json(body): Json<ScenePut>,
) -> Response {
    if !is_safe_segment(&room_id) {
        return (StatusCode::BAD_REQUEST, "invalid room_id").into_response();
    }

    let result = sqlx::query(
        "INSERT INTO scenes (room_id, scene_version, iv, ciphertext, updated_at) \
         VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(room_id) DO UPDATE SET \
           scene_version = excluded.scene_version, \
           iv = excluded.iv, \
           ciphertext = excluded.ciphertext, \
           updated_at = excluded.updated_at",
    )
    .bind(&room_id)
    .bind(body.scene_version)
    .bind(&body.iv)
    .bind(&body.ciphertext)
    .execute(&state.db)
    .await;

    match result {
        Ok(_) => {
            tracing::info!(room_id, scene_version = body.scene_version, "scene saved");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            tracing::error!(room_id, %err, "failed to save scene");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to save scene").into_response()
        }
    }
}

async fn get_scene(State(state): State<AppState>, Path(room_id): Path<String>) -> Response {
    if !is_safe_segment(&room_id) {
        return (StatusCode::BAD_REQUEST, "invalid room_id").into_response();
    }

    let row = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT scene_version, iv, ciphertext FROM scenes WHERE room_id = ?1",
    )
    .bind(&room_id)
    .fetch_optional(&state.db)
    .await;

    match row {
        Ok(Some((scene_version, iv, ciphertext))) => Json(SceneResponse {
            scene_version,
            iv,
            ciphertext,
        })
        .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "scene not found").into_response(),
        Err(err) => {
            tracing::error!(room_id, %err, "failed to load scene");
            (StatusCode::INTERNAL_SERVER_ERROR, "failed to load scene").into_response()
        }
    }
}

async fn put_file(state: &AppState, scope: &str, scope_id: &str, file_id: &str, body: Bytes) -> Response {
    if !is_safe_segment(scope_id) || !is_safe_segment(file_id) {
        return (StatusCode::BAD_REQUEST, "invalid path segment").into_response();
    }

    let dir = state.config.files_dir().join(scope).join(scope_id);
    if let Err(err) = tokio::fs::create_dir_all(&dir).await {
        tracing::error!(%err, scope, scope_id, "failed to create files directory");
        return (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response();
    }

    let path = dir.join(file_id);
    if let Err(err) = tokio::fs::write(&path, &body).await {
        tracing::error!(%err, path = %path.display(), "failed to write file");
        return (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response();
    }

    let size = body.len() as i64;
    let path_str = path.to_string_lossy().to_string();

    let result = sqlx::query(
        "INSERT INTO files (scope, scope_id, file_id, path, size, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, strftime('%Y-%m-%dT%H:%M:%fZ', 'now')) \
         ON CONFLICT(scope, scope_id, file_id) DO UPDATE SET \
           path = excluded.path, size = excluded.size, created_at = excluded.created_at",
    )
    .bind(scope)
    .bind(scope_id)
    .bind(file_id)
    .bind(&path_str)
    .bind(size)
    .execute(&state.db)
    .await;

    match result {
        Ok(_) => {
            tracing::info!(scope, scope_id, file_id, size, "file saved");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(err) => {
            tracing::error!(%err, "failed to record file metadata");
            (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response()
        }
    }
}

async fn get_file(state: &AppState, scope: &str, scope_id: &str, file_id: &str) -> Response {
    if !is_safe_segment(scope_id) || !is_safe_segment(file_id) {
        return (StatusCode::BAD_REQUEST, "invalid path segment").into_response();
    }

    let row = sqlx::query_as::<_, (String,)>(
        "SELECT path FROM files WHERE scope = ?1 AND scope_id = ?2 AND file_id = ?3",
    )
    .bind(scope)
    .bind(scope_id)
    .bind(file_id)
    .fetch_optional(&state.db)
    .await;

    let path = match row {
        Ok(Some((p,))) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(err) => {
            tracing::error!(%err, "failed to look up file metadata");
            return (StatusCode::INTERNAL_SERVER_ERROR, "storage error").into_response();
        }
    };

    match tokio::fs::read(&path).await {
        Ok(bytes) => (
            [(header::CACHE_CONTROL, "public, max-age=31536000")],
            bytes,
        )
            .into_response(),
        Err(err) => {
            tracing::error!(%err, path, "file metadata present but file missing on disk");
            (StatusCode::NOT_FOUND, "file not found").into_response()
        }
    }
}

async fn put_room_file(
    State(state): State<AppState>,
    Path((room_id, file_id)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    put_file(&state, "rooms", &room_id, &file_id, body).await
}

async fn get_room_file(
    State(state): State<AppState>,
    Path((room_id, file_id)): Path<(String, String)>,
) -> Response {
    get_file(&state, "rooms", &room_id, &file_id).await
}

async fn put_share_file(
    State(state): State<AppState>,
    Path((share_id, file_id)): Path<(String, String)>,
    body: Bytes,
) -> Response {
    put_file(&state, "share-links", &share_id, &file_id, body).await
}

async fn get_share_file(
    State(state): State<AppState>,
    Path((share_id, file_id)): Path<(String, String)>,
) -> Response {
    get_file(&state, "share-links", &share_id, &file_id).await
}
