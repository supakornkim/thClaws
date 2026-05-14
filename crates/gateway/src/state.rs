//! Shared `AppState`. Cheap clone (Arc inside).

use std::sync::Arc;

use crate::config::Config;
use crate::keys::{KeyStore, MemoryStore};

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

struct Inner {
    cfg: Config,
    keys: Arc<dyn KeyStore>,
    http: reqwest::Client,
}

impl AppState {
    pub async fn new(cfg: Config) -> anyhow::Result<Self> {
        let keys: Arc<dyn KeyStore> = match cfg.database_url.as_deref() {
            Some(url) if !url.is_empty() => {
                tracing::info!("connecting to Postgres");
                let pg = crate::keys::pg::PgStore::connect(url).await?;
                Arc::new(pg)
            }
            _ => {
                tracing::warn!(
                    "DATABASE_URL not set — running with in-memory key store (dev / stub mode)"
                );
                Arc::new(MemoryStore::new())
            }
        };
        let http = reqwest::Client::builder()
            .use_rustls_tls()
            .build()
            .map_err(anyhow::Error::from)?;
        Ok(Self(Arc::new(Inner { cfg, keys, http })))
    }

    /// Convenience for tests — inject an in-memory store directly.
    #[cfg(test)]
    pub fn with_memory(cfg: Config) -> Self {
        Self(Arc::new(Inner {
            cfg,
            keys: Arc::new(MemoryStore::new()),
            http: reqwest::Client::new(),
        }))
    }

    pub fn config(&self) -> &Config {
        &self.0.cfg
    }

    pub fn keys(&self) -> Arc<dyn KeyStore> {
        self.0.keys.clone()
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.0.http
    }
}
