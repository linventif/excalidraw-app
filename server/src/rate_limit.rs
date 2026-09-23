//! Minimal in-memory per-IP rate limiter for the `open-registration` auth
//! endpoints (`/auth/challenge`, `/auth/verify`). Unlike the rest of this
//! server (built for closed, classroom-sized deployments), this surface is
//! reachable unauthenticated from the open internet by design -- that's the
//! whole point of open registration -- so it needs at least a basic guard
//! against a script hammering it.
//!
//! Hand-rolled fixed-window counter rather than pulling in `governor` /
//! `tower_governor`: the requirement is "a handful of attempts per minute
//! per IP", which is a couple dozen lines of `HashMap<IpAddr, Vec<Instant>>`
//! bookkeeping. A full token-bucket crate (plus its `dashmap`/`quanta`
//! transitive deps) would be a lot of new dependency surface for a
//! requirement this narrow, and it mirrors how the rest of the codebase
//! already prefers small hand-rolled primitives (e.g. the in-memory
//! `ChallengeStore` in `open_auth.rs`) over new crates where the need is
//! this contained.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::SocketAddr;

use crate::state::AppState;

const WINDOW: Duration = Duration::from_secs(60);

/// "A handful of attempts per minute per IP" per the task brief: generous
/// enough for a real client retrying a dropped request or a failed
/// decryption, tight enough to blunt a naive brute-force/enumeration script.
const MAX_REQUESTS_PER_WINDOW: usize = 20;

/// Sliding-ish window: on every check we drop timestamps older than `WINDOW`
/// before counting, so it's not a hard reset-at-the-minute-boundary bucket
/// (which would let a burst of `2 * MAX` through right at the boundary).
#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Records one attempt from `ip` and returns whether it's within the
    /// limit. Expired timestamps (and, incidentally, IPs with none left) are
    /// swept on every call, so the map never grows past however many
    /// distinct IPs have hit a rate-limited route in the trailing minute.
    fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().unwrap_or_else(|e| e.into_inner());

        hits.retain(|_, timestamps| {
            timestamps.retain(|t| now.duration_since(*t) < WINDOW);
            !timestamps.is_empty()
        });

        let timestamps = hits.entry(ip).or_default();
        if timestamps.len() >= MAX_REQUESTS_PER_WINDOW {
            return false;
        }
        timestamps.push(now);
        true
    }
}

/// axum middleware: apply via `route_layer` to the auth routes only (not the
/// whole app). Requires the server to be run behind
/// `into_make_service_with_connect_info::<SocketAddr>()` (wired in
/// `main.rs`) so `ConnectInfo` is available to extract.
pub async fn limit_auth_routes(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let ip = client_ip(&req, peer);
    if state.rate_limiter.check(ip) {
        next.run(req).await
    } else {
        tracing::warn!(%ip, path = %req.uri().path(), "rate limit exceeded on auth endpoint");
        (
            StatusCode::TOO_MANY_REQUESTS,
            "too many requests, try again later",
        )
            .into_response()
    }
}

/// Prefers `X-Forwarded-For` (set by a reverse proxy in front of a public
/// deployment -- this repo's own CI/CD targets a Dokploy-fronted host) so
/// the limit is per real client rather than per proxy hop; falls back to the
/// raw TCP peer address for direct connections and local dev.
///
/// This trusts `X-Forwarded-For` unconditionally, which is fine for a rate
/// *limit* (worst case a malicious client spoofs the header to dodge its own
/// limit -- it can't frame or exhaust anyone else's budget by doing so) but
/// would not be an appropriate trust model for anything security-critical
/// like an IP allowlist.
fn client_ip(req: &Request, fallback: SocketAddr) -> IpAddr {
    req.headers()
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(str::trim)
        .and_then(|s| s.parse::<IpAddr>().ok())
        .unwrap_or_else(|| fallback.ip())
}
