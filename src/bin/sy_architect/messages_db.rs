use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_postgres::{Client, Config as PgConfig, NoTls, Row};

#[derive(Debug)]
pub enum MessagesDbError {
    InvalidUrl(String),
    Connect(tokio_postgres::Error),
    Query(tokio_postgres::Error),
}

impl fmt::Display for MessagesDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MessagesDbError::InvalidUrl(message) => write!(f, "invalid messages_db_url: {message}"),
            MessagesDbError::Connect(err) => match err.as_db_error() {
                Some(db) => write!(
                    f,
                    "messages_db connect failed: {} ({}): {}",
                    db.severity(),
                    db.code().code(),
                    db.message()
                ),
                None => write!(f, "messages_db connect failed: {err}"),
            },
            MessagesDbError::Query(err) => match err.as_db_error() {
                Some(db) => write!(
                    f,
                    "messages_db query failed: {} ({}): {}",
                    db.severity(),
                    db.code().code(),
                    db.message()
                ),
                None => write!(f, "messages_db query failed: {err}"),
            },
        }
    }
}

impl std::error::Error for MessagesDbError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimeWindow {
    #[serde(alias = "15m")]
    FifteenMinutes,
    #[serde(alias = "1h")]
    OneHour,
    #[serde(alias = "24h")]
    TwentyFourHours,
    All,
}

impl TimeWindow {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "15m" | "fifteen_minutes" => Some(TimeWindow::FifteenMinutes),
            "1h" | "one_hour" => Some(TimeWindow::OneHour),
            "24h" | "twenty_four_hours" => Some(TimeWindow::TwentyFourHours),
            "all" => Some(TimeWindow::All),
            _ => None,
        }
    }

    fn interval_clause(self) -> Option<&'static str> {
        match self {
            TimeWindow::FifteenMinutes => Some("now() - INTERVAL '15 minutes'"),
            TimeWindow::OneHour => Some("now() - INTERVAL '1 hour'"),
            TimeWindow::TwentyFourHours => Some("now() - INTERVAL '24 hours'"),
            TimeWindow::All => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct MessagesFilters {
    pub since: TimeWindow,
    pub with_error: Option<bool>,
}

impl Default for TimeWindow {
    fn default() -> Self {
        TimeWindow::All
    }
}

#[derive(Debug, Clone)]
pub struct MessagesCursor {
    pub received_at_iso: String,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesListItem {
    pub dedupe_key: String,
    pub subject: String,
    pub received_at: String,
    pub attempts: i32,
    pub processed_at: Option<String>,
    pub has_error: bool,
    pub size_bytes: i64,
    pub ich: Option<String>,
    pub thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesDetail {
    pub dedupe_key: String,
    pub subject: String,
    pub received_at: String,
    pub attempts: i32,
    pub processed_at: Option<String>,
    pub last_error: Option<String>,
    pub size_bytes: i64,
    pub ich: Option<String>,
    pub thread_id: Option<String>,
    pub payload_json: Option<Value>,
    pub payload_text: Option<String>,
}

pub struct MessagesDb {
    client: Client,
}

impl MessagesDb {
    /// Connect to the architect messages DB.
    ///
    /// `url` carries credentials + host/port (e.g.
    /// `postgresql://user:pass@host:5432`); any dbname embedded in it is
    /// ignored. `dbname` is the actual database to connect to (architect uses
    /// `fluxbee_storage` because it reads storage's inbox table). This split
    /// matches the Phase J' / Model D' contract for `resource_type=postgres`
    /// secrets: the secret carries only what is actually secret (creds +
    /// host); each consumer hardcodes the dbname it needs.
    pub async fn connect(url: &str, dbname: &str) -> Result<Self, MessagesDbError> {
        let mut config: PgConfig = url
            .parse()
            .map_err(|err: tokio_postgres::Error| MessagesDbError::InvalidUrl(err.to_string()))?;
        if !config.get_dbname().map(str::trim).unwrap_or("").is_empty() {
            return Err(MessagesDbError::InvalidUrl(format!(
                "postgres secret must not include a dbname (got '{}'); load only credentials + host (postgresql://user:pass@host:port)",
                config.get_dbname().unwrap_or("")
            )));
        }
        config.dbname(dbname);
        let (client, connection) = config
            .connect(NoTls)
            .await
            .map_err(MessagesDbError::Connect)?;
        tokio::spawn(async move {
            if let Err(err) = connection.await {
                tracing::warn!(error = %err, "messages_db connection task ended");
            }
        });
        Ok(Self { client })
    }

    pub async fn list_messages(
        &self,
        cursor: Option<&MessagesCursor>,
        filters: &MessagesFilters,
        limit: i64,
    ) -> Result<(Vec<MessagesListItem>, Option<MessagesCursor>), MessagesDbError> {
        let mut sql = String::from(
            "SELECT dedupe_key, subject, payload, \
                    to_char(received_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS received_at_iso, \
                    attempts, \
                    CASE WHEN processed_at IS NULL THEN NULL \
                         ELSE to_char(processed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') \
                    END AS processed_at_iso, \
                    last_error \
             FROM storage_inbox WHERE 1 = 1",
        );
        if let Some(interval) = filters.since.interval_clause() {
            sql.push_str(" AND received_at >= ");
            sql.push_str(interval);
        }
        let mut params_owned: Vec<String> = Vec::new();
        if let Some(cursor) = cursor {
            sql.push_str(" AND (received_at < $1::timestamptz OR (received_at = $1::timestamptz AND dedupe_key < $2))");
            params_owned.push(cursor.received_at_iso.clone());
            params_owned.push(cursor.dedupe_key.clone());
        }
        if let Some(with_error) = filters.with_error {
            if with_error {
                sql.push_str(" AND last_error IS NOT NULL");
            } else {
                sql.push_str(" AND last_error IS NULL");
            }
        }
        sql.push_str(" ORDER BY received_at DESC, dedupe_key DESC LIMIT ");
        sql.push_str(&limit.to_string());

        let params: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> =
            params_owned.iter().map(|s| s as _).collect();
        let rows = self
            .client
            .query(sql.as_str(), &params[..])
            .await
            .map_err(MessagesDbError::Query)?;

        let items: Vec<MessagesListItem> = rows.iter().map(row_to_list_item).collect();
        let next_cursor = items
            .last()
            .map(|last| MessagesCursor {
                received_at_iso: last.received_at.clone(),
                dedupe_key: last.dedupe_key.clone(),
            })
            .filter(|_| items.len() as i64 == limit);
        Ok((items, next_cursor))
    }

    pub async fn tail_since(
        &self,
        after: &MessagesCursor,
        limit: i64,
    ) -> Result<Vec<MessagesListItem>, MessagesDbError> {
        let sql = "SELECT dedupe_key, subject, payload, \
                          to_char(received_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS received_at_iso, \
                          attempts, \
                          CASE WHEN processed_at IS NULL THEN NULL \
                               ELSE to_char(processed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') \
                          END AS processed_at_iso, \
                          last_error \
                   FROM storage_inbox \
                   WHERE received_at > $1::timestamptz \
                      OR (received_at = $1::timestamptz AND dedupe_key > $2) \
                   ORDER BY received_at ASC, dedupe_key ASC \
                   LIMIT $3";
        let rows = self
            .client
            .query(sql, &[&after.received_at_iso, &after.dedupe_key, &limit])
            .await
            .map_err(MessagesDbError::Query)?;
        Ok(rows.iter().map(row_to_list_item).collect())
    }

    pub async fn get_message(
        &self,
        dedupe_key: &str,
    ) -> Result<Option<MessagesDetail>, MessagesDbError> {
        let sql = "SELECT dedupe_key, subject, payload, \
                          to_char(received_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') AS received_at_iso, \
                          attempts, \
                          CASE WHEN processed_at IS NULL THEN NULL \
                               ELSE to_char(processed_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.MS\"Z\"') \
                          END AS processed_at_iso, \
                          last_error \
                   FROM storage_inbox WHERE dedupe_key = $1";
        let rows = self
            .client
            .query(sql, &[&dedupe_key])
            .await
            .map_err(MessagesDbError::Query)?;
        Ok(rows.first().map(row_to_detail))
    }
}

fn row_to_list_item(row: &Row) -> MessagesListItem {
    let dedupe_key: String = row.get("dedupe_key");
    let subject: String = row.get("subject");
    let payload: Vec<u8> = row.get("payload");
    let received_at: String = row.get("received_at_iso");
    let attempts: i32 = row.get("attempts");
    let processed_at: Option<String> = row.get("processed_at_iso");
    let last_error: Option<String> = row.get("last_error");
    let size_bytes = payload.len() as i64;
    let (ich, thread_id) = extract_meta_identifiers(&payload);
    MessagesListItem {
        dedupe_key,
        subject,
        received_at,
        attempts,
        processed_at,
        has_error: last_error.is_some(),
        size_bytes,
        ich,
        thread_id,
    }
}

fn row_to_detail(row: &Row) -> MessagesDetail {
    let dedupe_key: String = row.get("dedupe_key");
    let subject: String = row.get("subject");
    let payload: Vec<u8> = row.get("payload");
    let received_at: String = row.get("received_at_iso");
    let attempts: i32 = row.get("attempts");
    let processed_at: Option<String> = row.get("processed_at_iso");
    let last_error: Option<String> = row.get("last_error");
    let size_bytes = payload.len() as i64;
    let (ich, thread_id) = extract_meta_identifiers(&payload);
    let (payload_json, payload_text) = decode_payload(&payload);
    MessagesDetail {
        dedupe_key,
        subject,
        received_at,
        attempts,
        processed_at,
        last_error,
        size_bytes,
        ich,
        thread_id,
        payload_json,
        payload_text,
    }
}

fn extract_meta_identifiers(payload: &[u8]) -> (Option<String>, Option<String>) {
    let Ok(text) = std::str::from_utf8(payload) else {
        return (None, None);
    };
    let Ok(value) = serde_json::from_str::<Value>(text) else {
        return (None, None);
    };
    let meta = value.get("meta");
    let ich = meta
        .and_then(|m| m.get("ich"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let thread_id = meta
        .and_then(|m| m.get("thread_id"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    (ich, thread_id)
}

fn decode_payload(payload: &[u8]) -> (Option<Value>, Option<String>) {
    let Ok(text) = std::str::from_utf8(payload) else {
        return (None, None);
    };
    match serde_json::from_str::<Value>(text) {
        Ok(value) => (Some(value), None),
        Err(_) => (None, Some(text.to_string())),
    }
}
