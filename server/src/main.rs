mod auth;
mod config;
mod db;
mod open_auth;
mod rate_limit;
mod rest;
mod state;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use axum::http::{header, HeaderValue, Method};
use axum::Router;
use socketioxide::SocketIo;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;

use config::{AuthMode, Config};
use state::AppState;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = match Config::from_env() {
        Ok(c) => c,
        Err(err) => {
            eprintln!("configuration error: {err}");
            std::process::exit(1);
        }
    };

    if let Err(err) = std::fs::create_dir_all(config.files_dir()) {
        eprintln!(
            "failed to create data directory {}: {err}",
            config.files_dir().display()
        );
        std::process::exit(1);
    }

    let db = match db::init_pool(&config).await {
        Ok(pool) => pool,
        Err(err) => {
            eprintln!("failed to initialize database: {err}");
            std::process::exit(1);
        }
    };

    tracing::info!(
        port = config.port,
        auth_mode = ?config.auth_mode,
        database_path = %config.database_path.display(),
        data_dir = %config.data_dir.display(),
        allowed_origins = ?config.allowed_origins,
        "starting excalidraw collab server"
    );

    let auth_mode = config.auth_mode;

    let state = AppState {
        db,
        config: Arc::new(config),
        challenge_store: open_auth::ChallengeStore::new(),
        rate_limiter: rate_limit::RateLimiter::new(),
    };

    let (socketio_layer, io) = SocketIo::builder().with_state(state.clone()).build_layer();

    ws::register(&io);

    let origins: Vec<HeaderValue> = state
        .config
        .allowed_origins
        .iter()
        .filter_map(|o| match o.parse::<HeaderValue>() {
            Ok(v) => Some(v),
            Err(err) => {
                tracing::warn!(origin = o, %err, "ignoring invalid ALLOWED_ORIGINS entry");
                None
            }
        })
        .collect();

    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET, Method::PUT, Method::POST, Method::OPTIONS])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    let protected = rest::protected_router().route_layer(axum::middleware::from_fn_with_state(
        state.clone(),
        auth::require_auth,
    ));

    // Comfortably above the client's 4 MiB FILE_UPLOAD_MAX_BYTES (encrypted
    // scenes/files gain some overhead from base64/AES-GCM framing) while
    // still bounding request size against abuse.
    const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

    let mut app = Router::new()
        .merge(rest::health_router())
        .merge(protected);

    // `/auth/challenge` and `/auth/verify` only exist at all in
    // open-registration mode -- in instance-token mode a request to them
    // 404s, which is simpler and more honest than routes that exist but
    // always answer "not enabled in this mode". Rate-limited (not
    // instance-token-gated, since this endpoint pair *is* the auth flow)
    // since it's the one surface of this server designed to be hit
    // unauthenticated from the open internet.
    if auth_mode == AuthMode::OpenRegistration {
        let open_auth_routes = open_auth::auth_router().route_layer(
            axum::middleware::from_fn_with_state(state.clone(), rate_limit::limit_auth_routes),
        );
        app = app.merge(open_auth_routes);
    }

    let app = app
        .with_state(state.clone())
        .layer(TraceLayer::new_for_http())
        .layer(socketio_layer)
        .layer(cors)
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES));

    let addr = format!("0.0.0.0:{}", state.config.port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(err) => {
            eprintln!("failed to bind {addr}: {err}");
            std::process::exit(1);
        }
    };

    tracing::info!(%addr, "listening");
    // `with_connect_info` so the per-IP rate limiter on the open-registration
    // auth routes can extract the real peer address via `ConnectInfo`; a
    // no-op for every other route, which don't use it.
    if let Err(err) = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    {
        eprintln!("server error: {err}");
        std::process::exit(1);
    }
}
