use std::env;
use std::path::PathBuf;

/// Origins the desktop app itself can present. Always allowed, in addition to
/// whatever the operator adds via `ALLOWED_ORIGINS`.
const BUILTIN_ORIGINS: &[&str] = &["tauri://localhost", "https://tauri.localhost"];

/// Default TTL for session tokens issued by `POST /auth/verify` in
/// `open-registration` mode. Overridable via `SESSION_TOKEN_TTL_SECONDS`.
const DEFAULT_SESSION_TOKEN_TTL_SECONDS: i64 = 86_400;

/// Which auth scheme this server instance enforces. See `AUTH_MODE` in
/// `Config::from_env` for the full contract of each mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMode {
    /// The original, default behavior: a single operator-shared
    /// `INSTANCE_TOKEN` gates every non-`/health` route. Closed, invite-only
    /// deployments (e.g. a classroom).
    InstanceToken,
    /// New: anyone can self-register a permanent identity via the
    /// `/auth/challenge` + `/auth/verify` public-key handshake and receive a
    /// session token, without ever needing an operator-shared secret. Meant
    /// for a public, open-internet instance.
    OpenRegistration,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub auth_mode: AuthMode,
    /// Required (and never empty) when `auth_mode == InstanceToken`.
    /// Optional when `auth_mode == OpenRegistration`: if the operator sets
    /// it anyway, it's accepted as an additional superuser bearer token
    /// alongside per-user session tokens, but it is not required.
    pub instance_token: Option<String>,
    /// TTL for session tokens issued in `open-registration` mode. Unused in
    /// `instance-token` mode.
    pub session_token_ttl_seconds: i64,
    pub database_path: PathBuf,
    pub data_dir: PathBuf,
    pub allowed_origins: Vec<String>,
}

impl Config {
    /// Loads configuration from environment variables.
    ///
    /// `AUTH_MODE` selects between two mutually exclusive auth schemes:
    /// - unset, or `"instance-token"` (the default): behavior is
    ///   byte-for-byte identical to the original single-shared-secret model.
    ///   `INSTANCE_TOKEN` is required and deliberately has no default: an
    ///   unconfigured self-hosted server must never accept traffic from
    ///   anyone on the network without an explicit shared secret.
    /// - `"open-registration"`: for a public, open-internet instance where
    ///   anyone can self-register (see `src/open_auth.rs`). `INSTANCE_TOKEN`
    ///   becomes optional in this mode.
    pub fn from_env() -> Result<Self, String> {
        let port = match env::var("PORT") {
            Ok(v) => v
                .parse::<u16>()
                .map_err(|_| format!("PORT must be a valid port number, got {v:?}"))?,
            Err(_) => 3002,
        };

        let auth_mode = match env::var("AUTH_MODE") {
            Ok(v) => match v.as_str() {
                "instance-token" => AuthMode::InstanceToken,
                "open-registration" => AuthMode::OpenRegistration,
                other => {
                    return Err(format!(
                        "AUTH_MODE must be \"instance-token\" or \"open-registration\", got {other:?}"
                    ))
                }
            },
            Err(_) => AuthMode::InstanceToken,
        };

        // Deliberately structured so the `InstanceToken` arm is exactly the
        // original (pre-open-registration) validation: same required-ness,
        // same error text, for zero behavior change in the default mode.
        let instance_token = match env::var("INSTANCE_TOKEN") {
            Ok(v) if !v.trim().is_empty() => Some(v),
            Ok(_) if auth_mode == AuthMode::InstanceToken => {
                return Err("INSTANCE_TOKEN must not be empty".to_string());
            }
            Ok(_) => None,
            Err(_) if auth_mode == AuthMode::InstanceToken => {
                return Err(
                    "INSTANCE_TOKEN environment variable is required (no default for security reasons). \
                     Set it to a long random shared secret, e.g. `openssl rand -hex 32`."
                        .to_string(),
                );
            }
            Err(_) => None,
        };

        let session_token_ttl_seconds = match env::var("SESSION_TOKEN_TTL_SECONDS") {
            Ok(v) => v.parse::<i64>().map_err(|_| {
                format!("SESSION_TOKEN_TTL_SECONDS must be a positive integer, got {v:?}")
            })?,
            Err(_) => DEFAULT_SESSION_TOKEN_TTL_SECONDS,
        };
        if session_token_ttl_seconds <= 0 {
            return Err("SESSION_TOKEN_TTL_SECONDS must be a positive integer".to_string());
        }

        let database_path = env::var("DATABASE_PATH")
            .unwrap_or_else(|_| "./data/excalidraw.sqlite".to_string())
            .into();

        let data_dir = env::var("DATA_DIR")
            .unwrap_or_else(|_| "./data".to_string())
            .into();

        let mut allowed_origins: Vec<String> =
            BUILTIN_ORIGINS.iter().map(|s| s.to_string()).collect();
        if let Ok(extra) = env::var("ALLOWED_ORIGINS") {
            for origin in extra.split(',') {
                let origin = origin.trim();
                if !origin.is_empty() && !allowed_origins.iter().any(|o| o == origin) {
                    allowed_origins.push(origin.to_string());
                }
            }
        }

        Ok(Self {
            port,
            auth_mode,
            instance_token,
            session_token_ttl_seconds,
            database_path,
            data_dir,
            allowed_origins,
        })
    }

    pub fn files_dir(&self) -> PathBuf {
        self.data_dir.join("files")
    }
}
