use sqlx::SqlitePool;
use std::sync::Arc;

use crate::config::Config;

/// Shared application state, cloned (cheaply, via `Arc`) into every axum
/// handler and every socket.io handler (via socketioxide's `State` extractor,
/// enabled with the `state` feature).
#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub config: Arc<Config>,
}
