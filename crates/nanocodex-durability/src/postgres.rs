use tokio_postgres::Client;

use crate::{OwnedState, OwnerId, OwnerToken, StateStore, StoreError, StoreFuture, StoredState};

const POSTGRES_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS nanocodex_durable_owners (
       state_id TEXT PRIMARY KEY,
       owner_id TEXT NOT NULL,
       fence NUMERIC(20, 0) NOT NULL
         CHECK (fence >= 1 AND fence <= 18446744073709551615)
     );
     CREATE TABLE IF NOT EXISTS nanocodex_durable_states (
       state_id TEXT PRIMARY KEY,
       revision NUMERIC(20, 0) NOT NULL
         CHECK (revision >= 1 AND revision <= 18446744073709551615),
       payload TEXT NOT NULL
     );";

/// Postgres-backed state store.
pub struct PostgresStore {
    client: Client,
}

impl PostgresStore {
    /// Initializes the current state schema using a caller-driven Postgres client.
    pub async fn new(client: Client) -> Result<Self, StoreError> {
        let mut store = Self { client };
        let transaction = store.client.transaction().await.map_err(backend)?;
        transaction
            .query(
                "SELECT pg_advisory_xact_lock(hashtextextended(
                   current_database() || ':' || current_schema() || ':nanocodex-durability-v2', 0
                 ))",
                &[],
            )
            .await
            .map_err(backend)?;
        transaction
            .batch_execute(POSTGRES_SCHEMA)
            .await
            .map_err(backend)?;
        validate_schema(&transaction).await?;
        transaction.commit().await.map_err(backend)?;
        Ok(store)
    }
}

impl StateStore for PostgresStore {
    fn acquire<'a>(
        &'a mut self,
        state_id: &'a str,
        owner_id: OwnerId,
    ) -> StoreFuture<'a, Result<OwnedState, StoreError>> {
        Box::pin(async move {
            let transaction = self.client.transaction().await.map_err(backend)?;
            let row = transaction
                .query_opt(
                    "INSERT INTO nanocodex_durable_owners (state_id, owner_id, fence)
                     VALUES ($1, $2, 1)
                     ON CONFLICT (state_id) DO UPDATE
                     SET owner_id = excluded.owner_id,
                         fence = nanocodex_durable_owners.fence + 1
                     WHERE nanocodex_durable_owners.fence < 18446744073709551615
                     RETURNING fence::text",
                    &[&state_id, &owner_id.as_str()],
                )
                .await
                .map_err(backend)?
                .ok_or_else(|| {
                    StoreError::NotCommitted("Postgres durability owner fence overflow".to_owned())
                })?;
            let fence = parse_u64(&row.get::<_, String>(0), "Postgres owner fence")?;
            let state = transaction
                .query_opt(
                    "SELECT revision::text, payload
                     FROM nanocodex_durable_states WHERE state_id = $1",
                    &[&state_id],
                )
                .await
                .map_err(backend)?
                .map(|row| {
                    Ok(StoredState {
                        revision: parse_u64(&row.get::<_, String>(0), "Postgres state revision")?,
                        payload: Some(row.get(1)),
                    })
                })
                .transpose()?
                .unwrap_or_default();
            let owner = OwnerToken::new(owner_id, fence);
            transaction.commit().await.map_err(backend)?;
            Ok(OwnedState { owner, state })
        })
    }

    fn replace<'a>(
        &'a mut self,
        state_id: &'a str,
        owner: &'a OwnerToken,
        expected_revision: u64,
        payload: &'a str,
    ) -> StoreFuture<'a, Result<u64, StoreError>> {
        Box::pin(async move {
            let transaction = self.client.transaction().await.map_err(backend)?;
            let retained_owner = transaction
                .query_opt(
                    "SELECT owner_id, fence::text FROM nanocodex_durable_owners
                     WHERE state_id = $1 FOR UPDATE",
                    &[&state_id],
                )
                .await
                .map_err(backend)?;
            let owns_state = match retained_owner {
                Some(row) => {
                    row.get::<_, String>(0) == owner.owner_id().as_str()
                        && parse_u64(&row.get::<_, String>(1), "Postgres owner fence")?
                            == owner.fence()
                }
                None => false,
            };
            if !owns_state {
                return Err(StoreError::Fenced);
            }
            let actual = transaction
                .query_opt(
                    "SELECT revision::text FROM nanocodex_durable_states
                     WHERE state_id = $1 FOR UPDATE",
                    &[&state_id],
                )
                .await
                .map_err(backend)?
                .map_or(Ok(0), |row| {
                    parse_u64(&row.get::<_, String>(0), "Postgres state revision")
                })?;
            if actual != expected_revision {
                return Err(StoreError::Conflict {
                    expected: expected_revision,
                    actual,
                });
            }
            let revision = actual.checked_add(1).ok_or_else(|| {
                StoreError::NotCommitted("Postgres durability revision overflow".to_owned())
            })?;
            let revision_text = revision.to_string();
            transaction
                .execute(
                    "INSERT INTO nanocodex_durable_states (state_id, revision, payload)
                     VALUES ($1, $2::numeric, $3)
                     ON CONFLICT (state_id) DO UPDATE
                     SET revision = excluded.revision, payload = excluded.payload",
                    &[&state_id, &revision_text, &payload],
                )
                .await
                .map_err(backend)?;
            transaction.commit().await.map_err(backend)?;
            Ok(revision)
        })
    }
}

async fn validate_schema(transaction: &tokio_postgres::Transaction<'_>) -> Result<(), StoreError> {
    let rows = transaction
        .query(
            "SELECT table_name, column_name, data_type, is_nullable,
                    numeric_precision, numeric_scale
             FROM information_schema.columns
             WHERE table_schema = current_schema()
               AND table_name IN ('nanocodex_durable_owners', 'nanocodex_durable_states')
             ORDER BY table_name, ordinal_position",
            &[],
        )
        .await
        .map_err(backend)?;
    let actual = rows
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, String>(3),
                row.get::<_, Option<i32>>(4),
                row.get::<_, Option<i32>>(5),
            )
        })
        .collect::<Vec<_>>();
    let expected = vec![
        (
            "nanocodex_durable_owners",
            "state_id",
            "text",
            "NO",
            None,
            None,
        ),
        (
            "nanocodex_durable_owners",
            "owner_id",
            "text",
            "NO",
            None,
            None,
        ),
        (
            "nanocodex_durable_owners",
            "fence",
            "numeric",
            "NO",
            Some(20),
            Some(0),
        ),
        (
            "nanocodex_durable_states",
            "state_id",
            "text",
            "NO",
            None,
            None,
        ),
        (
            "nanocodex_durable_states",
            "revision",
            "numeric",
            "NO",
            Some(20),
            Some(0),
        ),
        (
            "nanocodex_durable_states",
            "payload",
            "text",
            "NO",
            None,
            None,
        ),
    ];
    if actual.len() != expected.len()
        || actual.iter().zip(expected).any(|(actual, expected)| {
            actual.0 != expected.0
                || actual.1 != expected.1
                || actual.2 != expected.2
                || actual.3 != expected.3
                || actual.4 != expected.4
                || actual.5 != expected.5
        })
    {
        return Err(StoreError::Backend(
            "incompatible Postgres durability schema; recreate the two nanocodex_durable_* tables"
                .to_owned(),
        ));
    }
    let primary_keys = transaction
        .query(
            "SELECT tc.table_name, kcu.column_name, kcu.ordinal_position
             FROM information_schema.table_constraints AS tc
             JOIN information_schema.key_column_usage AS kcu
               ON tc.constraint_catalog = kcu.constraint_catalog
              AND tc.constraint_schema = kcu.constraint_schema
              AND tc.constraint_name = kcu.constraint_name
             WHERE tc.table_schema = current_schema()
               AND tc.constraint_type = 'PRIMARY KEY'
               AND tc.table_name IN ('nanocodex_durable_owners', 'nanocodex_durable_states')
             ORDER BY tc.table_name, kcu.ordinal_position",
            &[],
        )
        .await
        .map_err(backend)?;
    let primary_keys = primary_keys
        .into_iter()
        .map(|row| {
            (
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, i32>(2),
            )
        })
        .collect::<Vec<_>>();
    let expected_primary_keys = [
        ("nanocodex_durable_owners", "state_id", 1),
        ("nanocodex_durable_states", "state_id", 1),
    ];
    if primary_keys.len() != expected_primary_keys.len()
        || primary_keys
            .iter()
            .zip(expected_primary_keys)
            .any(|(actual, expected)| {
                actual.0 != expected.0 || actual.1 != expected.1 || actual.2 != expected.2
            })
    {
        return Err(StoreError::Backend(
            "incompatible Postgres durability primary keys; each state_id must be the sole primary key"
                .to_owned(),
        ));
    }
    Ok(())
}

fn parse_u64(value: &str, label: &str) -> Result<u64, StoreError> {
    value
        .parse()
        .map_err(|error| StoreError::Backend(format!("invalid {label}: {error}")))
}

fn backend(error: tokio_postgres::Error) -> StoreError {
    StoreError::Backend(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_reject_negative_and_oversized_text() {
        assert!(parse_u64("-1", "counter").is_err());
        assert!(parse_u64("18446744073709551616", "counter").is_err());
    }
}
