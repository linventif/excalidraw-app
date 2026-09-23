use axum::{
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::state::AppState;

/// axum middleware enforcing `Authorization: Bearer <INSTANCE_TOKEN>` on every
/// route it is attached to. `/health` is intentionally never wrapped with
/// this so Docker/reverse-proxy liveness probes don't need the token.
pub async fn require_instance_token(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path().to_string();
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());

    let token = header.and_then(|h| h.strip_prefix("Bearer "));

    match token {
        Some(t) if t == state.config.instance_token => next.run(req).await,
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
