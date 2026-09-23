use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use std::str::FromStr;

use crate::config::Config;

/// Opens (creating if needed) the SQLite database and runs migrations.
///
/// We use `sqlx::migrate!` with a `migrations/` folder (tracked via sqlx's
/// own `_sqlx_migrations` table) rather than hand-rolled `CREATE TABLE IF NOT
/// EXISTS` at startup: it's barely more code, and it means future schema
/// changes get a proper, ordered migration history instead of ad-hoc
/// `ALTER TABLE` guards sprinkled through `main`.
pub async fn init_pool(config: &Config) -> Result<SqlitePool, sqlx::Error> {
    if let Some(parent) = config.database_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|e| {
                sqlx::Error::Io(std::io::Error::other(format!(
                    "failed to create database directory {}: {e}",
                    parent.display()
                )))
            })?;
        }
    }

    let connect_options = SqliteConnectOptions::from_str(&format!(
        "sqlite://{}",
        config.database_path.display()
    ))?
    .create_if_missing(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(connect_options)
        .await?;

    sqlx::migrate!("./migrations").run(&pool).await?;

    Ok(pool)
}
