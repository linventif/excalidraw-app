use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::config::AuthMode;
use crate::open_auth;
use crate::state::AppState;

/// axum middleware enforcing `Authorization: Bearer <token>` on every route
/// it is attached to, with the accepted token(s) depending on `AUTH_MODE`.
/// `/health` is intentionally never wrapped with this so Docker/reverse-proxy
/// liveness probes don't need the token, and (in `open-registration` mode)
/// `/auth/challenge` / `/auth/verify` are never wrapped with this either,
/// since they *are* the auth flow.
///
/// In `instance-token` mode (the default) this is byte-for-byte the original
/// behavior: the sole valid token is `INSTANCE_TOKEN`.
///
/// In `open-registration` mode, a valid token is either:
/// - a still-valid session token issued by `POST /auth/verify`, or
/// - the operator's `INSTANCE_TOKEN`, if they set one anyway (treated as an
///   optional superuser secret rather than a requirement).
pub async fn require_auth(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let token = header.and_then(|h| h.strip_prefix("Bearer "));

    match state.config.auth_mode {
        AuthMode::InstanceToken => {
            // Unchanged from the original implementation: the only valid
            // token is INSTANCE_TOKEN. `from_env` guarantees this is `Some`
            // whenever `auth_mode == InstanceToken`.
            let instance_token = state
                .config
                .instance_token
                .as_deref()
                .expect("instance_token is required in instance-token mode");
            match token {
                Some(t) if t == instance_token => next.run(req).await,
                Some(_) => {
                    tracing::warn!(%path, "rejected request: wrong instance token");
                    (StatusCode::UNAUTHORIZED, "invalid instance token").into_response()
                }
                None => {
                    tracing::warn!(%path, "rejected request: missing Authorization header");
                    (
                        StatusCode::UNAUTHORIZED,
                        "missing Authorization: Bearer <token> header",
                    )
                        .into_response()
                }
            }
        }
        AuthMode::OpenRegistration => match token {
            Some(t) => {
                let is_instance_token = state.config.instance_token.as_deref() == Some(t);
                let is_valid_session =
                    is_instance_token || open_auth::validate_session_token(&state.db, t).await;
                if is_valid_session {
                    next.run(req).await
                } else {
                    tracing::warn!(%path, "rejected request: invalid or expired session token");
                    (
                        StatusCode::UNAUTHORIZED,
                        "invalid or expired session token",
                    )
                        .into_response()
                }
            }
            None => {
                tracing::warn!(%path, "rejected request: missing Authorization header");
                (
                    StatusCode::UNAUTHORIZED,
                    "missing Authorization: Bearer <token> header",
                )
                    .into_response()
            }
        },
    }
}
