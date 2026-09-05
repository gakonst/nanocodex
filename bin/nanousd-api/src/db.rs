use std::{
    path::Path,
    sync::{Arc, Mutex, MutexGuard},
    time::{SystemTime, UNIX_EPOCH},
};

use alloy_primitives::Address;
use nanousd::{CreditPackage, Order, OrderStatus};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};

#[derive(Clone)]
pub(crate) struct Database {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Debug, Clone)]
pub(crate) struct Fulfillment {
    pub id: String,
    pub wallet: Address,
    pub amount: u64,
    pub signed_transaction: Option<Vec<u8>>,
    pub transaction_hash: Option<String>,
    pub valid_before: Option<u64>,
}

impl Database {
    pub fn open(path: &Path) -> Result<Self, DbError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        connection.busy_timeout(std::time::Duration::from_secs(5))?;
        connection.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS orders (
                id TEXT PRIMARY KEY,
                token_hash BLOB NOT NULL,
                wallet TEXT NOT NULL,
                package_cents INTEGER NOT NULL,
                nanousd_units INTEGER NOT NULL,
                status TEXT NOT NULL,
                checkout_url TEXT,
                stripe_session_id TEXT UNIQUE,
                stripe_payment_intent_id TEXT,
                signed_transaction BLOB,
                transaction_hash TEXT,
                valid_before INTEGER,
                error TEXT,
                attempts INTEGER NOT NULL DEFAULT 0,
                next_attempt_at INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stripe_events (
                id TEXT PRIMARY KEY,
                event_type TEXT NOT NULL,
                received_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS orders_fulfillment
                ON orders(status, next_attempt_at, created_at);
            ",
        )?;
        ensure_order_column(&connection, "signed_transaction", "BLOB")?;
        ensure_order_column(&connection, "valid_before", "INTEGER")?;
        connection.execute(
            "UPDATE orders SET status = 'paid' WHERE status = 'fulfilling'",
            [],
        )?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    #[cfg(test)]
    pub fn memory() -> Result<Self, DbError> {
        Self::open(Path::new(":memory:"))
    }

    pub fn create_order(
        &self,
        id: &str,
        token_hash: &[u8],
        wallet: Address,
        package: CreditPackage,
        status: OrderStatus,
    ) -> Result<Order, DbError> {
        let now = unix_time()?;
        self.lock()?.execute(
            "INSERT INTO orders
             (id, token_hash, wallet, package_cents, nanousd_units, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            params![
                id,
                token_hash,
                wallet.to_string(),
                i64::try_from(package.usd_cents)?,
                i64::try_from(package.nanousd_units)?,
                status_name(status),
                i64::try_from(now)?,
            ],
        )?;
        self.order(id)?.ok_or(DbError::MissingOrder)
    }

    pub fn set_checkout(
        &self,
        id: &str,
        session_id: &str,
        checkout_url: &str,
    ) -> Result<(), DbError> {
        let changed = self.lock()?.execute(
            "UPDATE orders SET status = 'awaiting_payment', stripe_session_id = ?2,
             checkout_url = ?3, updated_at = ?4 WHERE id = ?1 AND status = 'created'",
            params![id, session_id, checkout_url, i64::try_from(unix_time()?)?],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(DbError::InvalidTransition)
        }
    }

    pub fn mark_mock_paid(&self, id: &str) -> Result<(), DbError> {
        let changed = self.lock()?.execute(
            "UPDATE orders SET status = 'paid', updated_at = ?2
             WHERE id = ?1 AND status = 'created'",
            params![id, i64::try_from(unix_time()?)?],
        )?;
        if changed == 1 {
            Ok(())
        } else {
            Err(DbError::InvalidTransition)
        }
    }

    pub fn record_stripe_payment(
        &self,
        event_id: &str,
        event_type: &str,
        order_id: &str,
        session_id: &str,
        payment_intent_id: Option<&str>,
    ) -> Result<bool, DbError> {
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = i64::try_from(unix_time()?)?;
        let inserted = transaction.execute(
            "INSERT OR IGNORE INTO stripe_events(id, event_type, received_at) VALUES (?1, ?2, ?3)",
            params![event_id, event_type, now],
        )?;
        if inserted == 0 {
            transaction.commit()?;
            return Ok(false);
        }
        let status = transaction
            .query_row(
                "SELECT status FROM orders WHERE id = ?1 AND stripe_session_id = ?2",
                params![order_id, session_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .ok_or(DbError::InvalidStripeOrder)?;
        let changed = transaction.execute(
            "UPDATE orders SET status = 'paid', stripe_payment_intent_id = ?3,
             updated_at = ?4, error = NULL
             WHERE id = ?1 AND stripe_session_id = ?2
             AND status IN ('awaiting_payment', 'failed')",
            params![order_id, session_id, payment_intent_id, now],
        )?;
        if changed != 1 && !matches!(status.as_str(), "paid" | "fulfilling" | "fulfilled") {
            return Err(DbError::InvalidStripeOrder);
        }
        transaction.commit()?;
        Ok(true)
    }

    pub fn order_authorized(&self, id: &str, token_hash: &[u8]) -> Result<Option<Order>, DbError> {
        let connection = self.lock()?;
        connection
            .query_row(
                "SELECT id, wallet, package_cents, nanousd_units, status, checkout_url,
                 transaction_hash, error, created_at, updated_at
                 FROM orders WHERE id = ?1 AND token_hash = ?2",
                params![id, token_hash],
                row_to_order,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn order(&self, id: &str) -> Result<Option<Order>, DbError> {
        self.lock()?
            .query_row(
                "SELECT id, wallet, package_cents, nanousd_units, status, checkout_url,
                 transaction_hash, error, created_at, updated_at FROM orders WHERE id = ?1",
                [id],
                row_to_order,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn claim_fulfillment(&self) -> Result<Option<Fulfillment>, DbError> {
        let now = i64::try_from(unix_time()?)?;
        let mut connection = self.lock()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id = transaction
            .query_row(
                "SELECT id FROM orders WHERE status IN ('paid', 'failed')
                 AND next_attempt_at <= ?1 ORDER BY created_at LIMIT 1",
                [now],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(id) = id else {
            transaction.commit()?;
            return Ok(None);
        };
        transaction.execute(
            "UPDATE orders SET status = 'fulfilling', attempts = attempts + 1,
             updated_at = ?2, error = NULL WHERE id = ?1",
            params![id, now],
        )?;
        let result = transaction.query_row(
            "SELECT id, wallet, nanousd_units, signed_transaction, transaction_hash, valid_before
             FROM orders WHERE id = ?1",
            [&id],
            |row| {
                let wallet = row.get::<_, String>(1)?;
                let amount = row.get::<_, i64>(2)?;
                Ok((
                    row.get::<_, String>(0)?,
                    wallet,
                    amount,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                ))
            },
        )?;
        transaction.commit()?;
        Ok(Some(Fulfillment {
            id: result.0,
            wallet: result.1.parse().map_err(|_| DbError::InvalidAddress)?,
            amount: u64::try_from(result.2)?,
            signed_transaction: result.3,
            transaction_hash: result.4,
            valid_before: result.5.map(u64::try_from).transpose()?,
        }))
    }

    pub fn save_prepared_transaction(
        &self,
        id: &str,
        signed_transaction: &[u8],
        transaction_hash: &str,
        valid_before: u64,
    ) -> Result<(), DbError> {
        self.lock()?.execute(
            "UPDATE orders SET signed_transaction = ?2, transaction_hash = ?3,
             valid_before = ?4, updated_at = ?5
             WHERE id = ?1 AND status = 'fulfilling'",
            params![
                id,
                signed_transaction,
                transaction_hash,
                i64::try_from(valid_before)?,
                i64::try_from(unix_time()?)?
            ],
        )?;
        Ok(())
    }

    pub fn mark_fulfilled(&self, id: &str, transaction_hash: &str) -> Result<(), DbError> {
        self.lock()?.execute(
            "UPDATE orders SET status = 'fulfilled', transaction_hash = ?2,
             updated_at = ?3, error = NULL WHERE id = ?1 AND status = 'fulfilling'",
            params![id, transaction_hash, i64::try_from(unix_time()?)?],
        )?;
        Ok(())
    }

    pub fn mark_failed(&self, id: &str, error: &str, retry_after: u64) -> Result<(), DbError> {
        let now = unix_time()?;
        self.lock()?.execute(
            "UPDATE orders SET status = 'failed', error = ?2, next_attempt_at = ?3,
             updated_at = ?4 WHERE id = ?1 AND status = 'fulfilling'",
            params![
                id,
                error,
                i64::try_from(now.saturating_add(retry_after))?,
                i64::try_from(now)?
            ],
        )?;
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>, DbError> {
        self.connection.lock().map_err(|_| DbError::Poisoned)
    }
}

fn ensure_order_column(connection: &Connection, name: &str, sql_type: &str) -> Result<(), DbError> {
    let mut statement = connection.prepare("PRAGMA table_info(orders)")?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for column in columns {
        if column? == name {
            return Ok(());
        }
    }
    drop(statement);
    connection.execute(
        &format!("ALTER TABLE orders ADD COLUMN {name} {sql_type}"),
        [],
    )?;
    Ok(())
}

fn row_to_order(row: &rusqlite::Row<'_>) -> rusqlite::Result<Order> {
    let wallet = row.get::<_, String>(1)?;
    let status = row.get::<_, String>(4)?;
    Ok(Order {
        id: row.get(0)?,
        wallet: wallet.parse().map_err(|_| rusqlite::Error::InvalidQuery)?,
        package: CreditPackage {
            usd_cents: u64::try_from(row.get::<_, i64>(2)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
            nanousd_units: u64::try_from(row.get::<_, i64>(3)?)
                .map_err(|_| rusqlite::Error::InvalidQuery)?,
        },
        status: parse_status(&status).map_err(|()| rusqlite::Error::InvalidQuery)?,
        checkout_url: row.get(5)?,
        transaction_hash: row.get(6)?,
        error: row.get(7)?,
        created_at: u64::try_from(row.get::<_, i64>(8)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        updated_at: u64::try_from(row.get::<_, i64>(9)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}

const fn status_name(status: OrderStatus) -> &'static str {
    match status {
        OrderStatus::Created => "created",
        OrderStatus::AwaitingPayment => "awaiting_payment",
        OrderStatus::Paid => "paid",
        OrderStatus::Fulfilling => "fulfilling",
        OrderStatus::Fulfilled => "fulfilled",
        OrderStatus::Failed => "failed",
        OrderStatus::Expired => "expired",
    }
}

fn parse_status(status: &str) -> Result<OrderStatus, ()> {
    match status {
        "created" => Ok(OrderStatus::Created),
        "awaiting_payment" => Ok(OrderStatus::AwaitingPayment),
        "paid" => Ok(OrderStatus::Paid),
        "fulfilling" => Ok(OrderStatus::Fulfilling),
        "fulfilled" => Ok(OrderStatus::Fulfilled),
        "failed" => Ok(OrderStatus::Failed),
        "expired" => Ok(OrderStatus::Expired),
        _ => Err(()),
    }
}

fn unix_time() -> Result<u64, DbError> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DbError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("database path error: {0}")]
    Io(#[from] std::io::Error),
    #[error("system clock is before the Unix epoch")]
    Clock(#[from] std::time::SystemTimeError),
    #[error("database integer is out of range")]
    Integer(#[from] std::num::TryFromIntError),
    #[error("database lock was poisoned")]
    Poisoned,
    #[error("order does not exist after insertion")]
    MissingOrder,
    #[error("order state transition was rejected")]
    InvalidTransition,
    #[error("Stripe event did not match its order")]
    InvalidStripeOrder,
    #[error("database contains an invalid wallet address")]
    InvalidAddress,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_signed_transaction_columns_to_an_existing_database() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("orders.sqlite3");
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE orders (
                    id TEXT PRIMARY KEY,
                    token_hash BLOB NOT NULL,
                    wallet TEXT NOT NULL,
                    package_cents INTEGER NOT NULL,
                    nanousd_units INTEGER NOT NULL,
                    status TEXT NOT NULL,
                    checkout_url TEXT,
                    stripe_session_id TEXT UNIQUE,
                    stripe_payment_intent_id TEXT,
                    transaction_hash TEXT,
                    error TEXT,
                    attempts INTEGER NOT NULL DEFAULT 0,
                    next_attempt_at INTEGER NOT NULL DEFAULT 0,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );",
            )
            .unwrap();
        drop(connection);

        let database = Database::open(&path).unwrap();
        let connection = database.lock().unwrap();
        let mut statement = connection.prepare("PRAGMA table_info(orders)").unwrap();
        let columns: Vec<String> = statement
            .query_map([], |row| row.get(1))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert!(columns.iter().any(|column| column == "signed_transaction"));
        assert!(columns.iter().any(|column| column == "valid_before"));
    }

    #[test]
    fn prepared_transaction_survives_a_fulfillment_retry() {
        let database = Database::memory().unwrap();
        database
            .create_order(
                "ord_retry",
                &[9; 32],
                Address::repeat_byte(0x24),
                CreditPackage::from_cents(500),
                OrderStatus::Paid,
            )
            .unwrap();
        let fulfillment = database.claim_fulfillment().unwrap().unwrap();
        database
            .save_prepared_transaction(
                &fulfillment.id,
                &[0x76, 0x01, 0x02],
                "0x1234",
                1_900_000_000,
            )
            .unwrap();
        database.mark_failed(&fulfillment.id, "retry", 0).unwrap();

        let retry = database.claim_fulfillment().unwrap().unwrap();
        assert_eq!(retry.signed_transaction.unwrap(), [0x76, 0x01, 0x02]);
        assert_eq!(retry.transaction_hash.as_deref(), Some("0x1234"));
        assert_eq!(retry.valid_before, Some(1_900_000_000));
    }

    #[test]
    fn stripe_redelivery_does_not_regress_or_reject_fulfilled_order() {
        let database = Database::memory().unwrap();
        let wallet = Address::repeat_byte(0x42);
        database
            .create_order(
                "ord_test",
                &[7; 32],
                wallet,
                CreditPackage::from_cents(500),
                OrderStatus::Created,
            )
            .unwrap();
        database
            .set_checkout("ord_test", "cs_test", "https://checkout.stripe.test")
            .unwrap();

        assert!(
            database
                .record_stripe_payment(
                    "evt_1",
                    "checkout.session.completed",
                    "ord_test",
                    "cs_test",
                    Some("pi_test"),
                )
                .unwrap()
        );
        let fulfillment = database.claim_fulfillment().unwrap().unwrap();
        database.mark_fulfilled(&fulfillment.id, "0x1234").unwrap();

        assert!(
            database
                .record_stripe_payment(
                    "evt_2",
                    "checkout.session.async_payment_succeeded",
                    "ord_test",
                    "cs_test",
                    Some("pi_test"),
                )
                .unwrap()
        );
        assert_eq!(
            database.order("ord_test").unwrap().unwrap().status,
            OrderStatus::Fulfilled
        );
        assert!(
            !database
                .record_stripe_payment(
                    "evt_2",
                    "checkout.session.async_payment_succeeded",
                    "ord_test",
                    "cs_test",
                    Some("pi_test"),
                )
                .unwrap()
        );
    }
}
