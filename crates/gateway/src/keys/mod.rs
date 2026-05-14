//! Personal access keys. Single backend trait with two impls:
//! `PgStore` (production) and `MemoryStore` (dev / unit tests).
//!
//! ## Storage shape
//!
//! Keys are random 32-byte tokens base64url-encoded ("gw_v1_<base64>")
//! and stored as their SHA-256 hash — the plaintext is shown to the
//! user exactly once at mint time. Lookup hashes the inbound Bearer
//! and SELECTs the matching row.
//!
//! ## Columns (Postgres)
//!
//! ```sql
//! CREATE TABLE access_keys (
//!     id         UUID PRIMARY KEY,
//!     hash       BYTEA NOT NULL UNIQUE,
//!     user_sub   TEXT NOT NULL,
//!     label      TEXT,
//!     created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
//!     last_used  TIMESTAMPTZ,
//!     revoked_at TIMESTAMPTZ
//! );
//! CREATE INDEX access_keys_user_sub_idx ON access_keys (user_sub);
//! ```

use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine as _};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

pub mod pg;

#[derive(Debug, Clone)]
pub struct AccessKeyRecord {
    pub id: Uuid,
    pub user_sub: String,
    pub label: Option<String>,
    pub created_at: DateTime<Utc>,
    pub last_used: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyStoreError {
    #[error("backend: {0}")]
    Backend(String),
}

#[async_trait::async_trait]
pub trait KeyStore: Send + Sync + 'static {
    /// Insert a fresh key. The hash is computed by the caller.
    async fn mint(
        &self,
        hash: [u8; 32],
        user_sub: &str,
        label: Option<&str>,
    ) -> Result<AccessKeyRecord, KeyStoreError>;

    /// Lookup by hash. Returns None if the key doesn't exist or has
    /// been revoked. Touches `last_used` on hit.
    async fn lookup_and_touch(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<AccessKeyRecord>, KeyStoreError>;

    /// List a user's non-revoked keys, newest first.
    async fn list_for_user(&self, user_sub: &str) -> Result<Vec<AccessKeyRecord>, KeyStoreError>;

    /// Revoke a key. No-op if the key doesn't exist or is already
    /// revoked. Returns whether anything changed.
    async fn revoke(&self, id: Uuid, user_sub: &str) -> Result<bool, KeyStoreError>;
}

/// Generate a 32-byte random key, return `(plaintext, hash)`. The
/// plaintext is what the gateway hands back to the user — never
/// persisted, never reconstructable.
pub fn mint_key() -> (String, [u8; 32]) {
    let mut buf = [0u8; 32];
    getrandom::getrandom(&mut buf).expect("getrandom");
    let plaintext = format!("gw_v1_{}", B64.encode(buf));
    let hash = Sha256::digest(plaintext.as_bytes());
    (plaintext, hash.into())
}

/// Hash an inbound key for lookup. Must match `mint_key`'s scheme.
pub fn hash_key(plaintext: &str) -> [u8; 32] {
    Sha256::digest(plaintext.as_bytes()).into()
}

#[derive(Default)]
pub struct MemoryStore {
    inner: Arc<Mutex<MemoryInner>>,
}

#[derive(Default)]
struct MemoryInner {
    rows: Vec<MemoryRow>,
}

#[derive(Clone)]
struct MemoryRow {
    id: Uuid,
    hash: [u8; 32],
    user_sub: String,
    label: Option<String>,
    created_at: DateTime<Utc>,
    last_used: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl KeyStore for MemoryStore {
    async fn mint(
        &self,
        hash: [u8; 32],
        user_sub: &str,
        label: Option<&str>,
    ) -> Result<AccessKeyRecord, KeyStoreError> {
        let mut inner = self.inner.lock().unwrap();
        let row = MemoryRow {
            id: Uuid::new_v4(),
            hash,
            user_sub: user_sub.to_string(),
            label: label.map(String::from),
            created_at: Utc::now(),
            last_used: None,
            revoked_at: None,
        };
        let record = AccessKeyRecord {
            id: row.id,
            user_sub: row.user_sub.clone(),
            label: row.label.clone(),
            created_at: row.created_at,
            last_used: row.last_used,
        };
        inner.rows.push(row);
        Ok(record)
    }

    async fn lookup_and_touch(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<AccessKeyRecord>, KeyStoreError> {
        let mut inner = self.inner.lock().unwrap();
        let now = Utc::now();
        for row in inner.rows.iter_mut() {
            if &row.hash == hash && row.revoked_at.is_none() {
                row.last_used = Some(now);
                return Ok(Some(AccessKeyRecord {
                    id: row.id,
                    user_sub: row.user_sub.clone(),
                    label: row.label.clone(),
                    created_at: row.created_at,
                    last_used: row.last_used,
                }));
            }
        }
        Ok(None)
    }

    async fn list_for_user(&self, user_sub: &str) -> Result<Vec<AccessKeyRecord>, KeyStoreError> {
        let inner = self.inner.lock().unwrap();
        let mut out: Vec<AccessKeyRecord> = inner
            .rows
            .iter()
            .filter(|r| r.user_sub == user_sub && r.revoked_at.is_none())
            .map(|r| AccessKeyRecord {
                id: r.id,
                user_sub: r.user_sub.clone(),
                label: r.label.clone(),
                created_at: r.created_at,
                last_used: r.last_used,
            })
            .collect();
        out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(out)
    }

    async fn revoke(&self, id: Uuid, user_sub: &str) -> Result<bool, KeyStoreError> {
        let mut inner = self.inner.lock().unwrap();
        for row in inner.rows.iter_mut() {
            if row.id == id && row.user_sub == user_sub && row.revoked_at.is_none() {
                row.revoked_at = Some(Utc::now());
                return Ok(true);
            }
        }
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mint_key_returns_prefixed_plaintext_and_matching_hash() {
        let (plaintext, hash) = mint_key();
        assert!(plaintext.starts_with("gw_v1_"));
        assert_eq!(hash_key(&plaintext), hash);
    }

    #[test]
    fn distinct_mints_produce_distinct_keys() {
        let (a, _) = mint_key();
        let (b, _) = mint_key();
        assert_ne!(a, b);
    }

    #[tokio::test]
    async fn memory_store_lookup_returns_none_for_unknown() {
        let store = MemoryStore::new();
        let (_, hash) = mint_key();
        assert!(store.lookup_and_touch(&hash).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn memory_store_mint_and_lookup_round_trip() {
        let store = MemoryStore::new();
        let (plaintext, hash) = mint_key();
        let minted = store.mint(hash, "user-1", Some("desktop")).await.unwrap();
        assert_eq!(minted.user_sub, "user-1");

        let found = store
            .lookup_and_touch(&hash_key(&plaintext))
            .await
            .unwrap()
            .expect("found");
        assert_eq!(found.user_sub, "user-1");
        assert_eq!(found.label.as_deref(), Some("desktop"));
        assert!(found.last_used.is_some());
    }

    #[tokio::test]
    async fn memory_store_list_filters_to_user_and_excludes_revoked() {
        let store = MemoryStore::new();
        let (_, h1) = mint_key();
        let (_, h2) = mint_key();
        let (_, h3) = mint_key();
        let a = store.mint(h1, "u1", Some("a")).await.unwrap();
        let _b = store.mint(h2, "u1", Some("b")).await.unwrap();
        let _c = store.mint(h3, "u2", None).await.unwrap();

        assert!(store.revoke(a.id, "u1").await.unwrap());
        // Revoking a stranger's key returns false.
        assert!(!store.revoke(a.id, "u_other").await.unwrap());

        let listed = store.list_for_user("u1").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label.as_deref(), Some("b"));
    }

    #[tokio::test]
    async fn memory_store_revoked_key_is_invisible_to_lookup() {
        let store = MemoryStore::new();
        let (plaintext, hash) = mint_key();
        let row = store.mint(hash, "u1", None).await.unwrap();
        store.revoke(row.id, "u1").await.unwrap();
        assert!(store
            .lookup_and_touch(&hash_key(&plaintext))
            .await
            .unwrap()
            .is_none());
    }
}
