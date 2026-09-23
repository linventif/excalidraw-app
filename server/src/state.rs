use sqlx::SqlitePool;
use std::sync::Arc;

use crate::config::Config;
use crate::open_auth::ChallengeStore;
use crate::rate_limit::RateLimiter;

/// Shared application state, cloned (cheaply, via `Arc`) into every axum
/// handler and every socket.io handler (via socketioxide's `State` extractor,
/// enabled with the `state` feature).
///
/// `challenge_store` and `rate_limiter` are always present, even in
/// `instance-token` mode -- they're cheap, unused, empty structures in that
/// mode since nothing ever calls the `open-registration`-only auth routes.
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
    pub challenge_store: Arc<ChallengeStore>,
    pub rate_limiter: Arc<RateLimiter>,
}
