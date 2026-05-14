//! Postgres-backed [`KeyStore`]. Connects via `sqlx::PgPool`.
//!
//! Schema migration is intentionally inline (not `sqlx::migrate!`)
//! so the gateway can `CREATE TABLE IF NOT EXISTS` on first start
//! without an embedded migrations dir. A future split into proper
//! migrations is tracked as a follow-up if the schema grows.

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::{AccessKeyRecord, KeyStore, KeyStoreError};

pub struct PgStore {
    pool: PgPool,
}

impl PgStore {
    pub async fn connect(database_url: &str) -> Result<Self, sqlx::Error> {
        let pool = PgPool::connect(database_url).await?;
        let store = Self { pool };
        store.ensure_schema().await?;
        Ok(store)
    }

    async fn ensure_schema(&self) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            CREATE TABLE IF NOT EXISTS access_keys (
                id         UUID PRIMARY KEY,
                hash       BYTEA NOT NULL UNIQUE,
                user_sub   TEXT NOT NULL,
                label      TEXT,
                created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
                last_used  TIMESTAMPTZ,
                revoked_at TIMESTAMPTZ
            )
            "#,
        )
        .execute(&self.pool)
        .await?;
        sqlx::query(
            "CREATE INDEX IF NOT EXISTS access_keys_user_sub_idx ON access_keys (user_sub)",
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl KeyStore for PgStore {
    async fn mint(
        &self,
        hash: [u8; 32],
        user_sub: &str,
        label: Option<&str>,
    ) -> Result<AccessKeyRecord, KeyStoreError> {
        let id = Uuid::new_v4();
        let row: (DateTime<Utc>,) = sqlx::query_as(
            r#"INSERT INTO access_keys (id, hash, user_sub, label)
               VALUES ($1, $2, $3, $4)
               RETURNING created_at"#,
        )
        .bind(id)
        .bind(&hash[..])
        .bind(user_sub)
        .bind(label)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Backend(e.to_string()))?;
        Ok(AccessKeyRecord {
            id,
            user_sub: user_sub.to_string(),
            label: label.map(String::from),
            created_at: row.0,
            last_used: None,
        })
    }

    async fn lookup_and_touch(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<AccessKeyRecord>, KeyStoreError> {
        let row: Option<(
            Uuid,
            String,
            Option<String>,
            DateTime<Utc>,
            Option<DateTime<Utc>>,
        )> = sqlx::query_as(
            r#"UPDATE access_keys
                   SET last_used = now()
                   WHERE hash = $1 AND revoked_at IS NULL
                   RETURNING id, user_sub, label, created_at, last_used"#,
        )
        .bind(&hash[..])
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Backend(e.to_string()))?;
        Ok(row.map(
            |(id, user_sub, label, created_at, last_used)| AccessKeyRecord {
                id,
                user_sub,
                label,
                created_at,
                last_used,
            },
        ))
    }

    async fn list_for_user(&self, user_sub: &str) -> Result<Vec<AccessKeyRecord>, KeyStoreError> {
        let rows: Vec<(Uuid, Option<String>, DateTime<Utc>, Option<DateTime<Utc>>)> =
            sqlx::query_as(
                r#"SELECT id, label, created_at, last_used
                   FROM access_keys
                   WHERE user_sub = $1 AND revoked_at IS NULL
                   ORDER BY created_at DESC"#,
            )
            .bind(user_sub)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| KeyStoreError::Backend(e.to_string()))?;
        Ok(rows
            .into_iter()
            .map(|(id, label, created_at, last_used)| AccessKeyRecord {
                id,
                user_sub: user_sub.to_string(),
                label,
                created_at,
                last_used,
            })
            .collect())
    }

    async fn revoke(&self, id: Uuid, user_sub: &str) -> Result<bool, KeyStoreError> {
        let affected = sqlx::query(
            r#"UPDATE access_keys
               SET revoked_at = now()
               WHERE id = $1 AND user_sub = $2 AND revoked_at IS NULL"#,
        )
        .bind(id)
        .bind(user_sub)
        .execute(&self.pool)
        .await
        .map_err(|e| KeyStoreError::Backend(e.to_string()))?;
        Ok(affected.rows_affected() > 0)
    }
}
