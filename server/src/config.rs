use std::env;
use std::path::PathBuf;

/// Origins the desktop app itself can present. Always allowed, in addition to
/// whatever the operator adds via `ALLOWED_ORIGINS`.
const BUILTIN_ORIGINS: &[&str] = &["tauri://localhost", "https://tauri.localhost"];

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub instance_token: String,
    pub database_path: PathBuf,
    pub data_dir: PathBuf,
    pub allowed_origins: Vec<String>,
}

impl Config {
    /// Loads configuration from environment variables.
    ///
    /// `INSTANCE_TOKEN` is required and deliberately has no default: an
    /// unconfigured self-hosted server must never accept traffic from anyone
    /// on the network without an explicit shared secret.
    pub fn from_env() -> Result<Self, String> {
        let port = match env::var("PORT") {
            Ok(v) => v
                .parse::<u16>()
                .map_err(|_| format!("PORT must be a valid port number, got {v:?}"))?,
            Err(_) => 3002,
        };

        let instance_token = env::var("INSTANCE_TOKEN").map_err(|_| {
            "INSTANCE_TOKEN environment variable is required (no default for security reasons). \
             Set it to a long random shared secret, e.g. `openssl rand -hex 32`."
                .to_string()
        })?;
        if instance_token.trim().is_empty() {
            return Err("INSTANCE_TOKEN must not be empty".to_string());
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
            instance_token,
            database_path,
            data_dir,
            allowed_origins,
        })
    }

    pub fn files_dir(&self) -> PathBuf {
        self.data_dir.join("files")
    }
}
