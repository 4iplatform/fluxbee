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
}

impl Default for TimeWindow {
    fn default() -> Self {
        TimeWindow::All
    }
}

/// A position in the log: `ts` orders the list (when the router copied the message), `stored_at`
/// orders the live tail (when SY.storage wrote it: a stream's batch can arrive late, A-70).
#[derive(Debug, Clone)]
pub struct MessagesCursor {
    pub at_iso: String,
    pub dedupe_key: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesListItem {
    pub dedupe_key: String,
    pub ts: String,
    pub stored_at: String,
    pub msg_type: String,
    /// The sender's name as its router stamped it; its ILK on a message copied before routers
    /// stamped names (the rows backfilled at 0.1.76).
    pub from: String,
    /// The destination as routed (a node name, a UUID, `broadcast`), else the destination ILK.
    pub to: Option<String>,
    pub ich: Option<String>,
    pub thread_id: Option<String>,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MessagesDetail {
    pub dedupe_key: String,
    pub ts: String,
    pub stored_at: String,
    pub msg_type: String,
    pub from: String,
    pub to: Option<String>,
    pub from_ilk: String,
    pub to_ilk: Option<String>,
    pub ich: Option<String>,
    pub thread_id: Option<String>,
    pub trace_id: Option<String>,
    pub tags: Vec<String>,
    pub size_bytes: i64,
    /// The whole message as the router copied it.
    pub message: Value,
    pub content: Value,
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
    /// `fluxbee_storage` because it reads storage's message log, `turns`). This
    /// split matches the Phase J' / Model D' contract for `resource_type=postgres`
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

    /// The log, newest first by `ts`, a page at a time. Also returns where the live tail starts:
    /// the last row written, read first, so a row written meanwhile is in the tail (the page drops
    /// a row it already shows) instead of in neither.
    pub async fn list_messages(
        &self,
        cursor: Option<&MessagesCursor>,
        filters: &MessagesFilters,
        limit: i64,
    ) -> Result<
        (
            Vec<MessagesListItem>,
            Option<MessagesCursor>,
            Option<MessagesCursor>,
        ),
        MessagesDbError,
    > {
        let tail_cursor = self
            .client
            .query_opt(
                "SELECT to_char(stored_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
                        AS stored_at_iso, dedupe_key \
                 FROM turns ORDER BY stored_at DESC, dedupe_key DESC LIMIT 1",
                &[],
            )
            .await
            .map_err(MessagesDbError::Query)?
            .map(|row| MessagesCursor {
                at_iso: row.get("stored_at_iso"),
                dedupe_key: row.get("dedupe_key"),
            });
        let mut sql = format!("SELECT {LIST_COLUMNS} FROM turns WHERE 1 = 1");
        if let Some(interval) = filters.since.interval_clause() {
            sql.push_str(" AND ts >= ");
            sql.push_str(interval);
        }
        let mut params_owned: Vec<String> = Vec::new();
        if let Some(cursor) = cursor {
            sql.push_str(
                " AND (ts < $1::text::timestamptz OR (ts = $1::text::timestamptz AND dedupe_key < $2))",
            );
            params_owned.push(cursor.at_iso.clone());
            params_owned.push(cursor.dedupe_key.clone());
        }
        sql.push_str(" ORDER BY ts DESC, dedupe_key DESC LIMIT ");
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
                at_iso: last.ts.clone(),
                dedupe_key: last.dedupe_key.clone(),
            })
            .filter(|_| items.len() as i64 == limit);
        Ok((items, next_cursor, tail_cursor))
    }

    /// What was written after `after`, in the order it was written.
    pub async fn tail_since(
        &self,
        after: &MessagesCursor,
        limit: i64,
    ) -> Result<Vec<MessagesListItem>, MessagesDbError> {
        let sql = format!(
            "SELECT {LIST_COLUMNS} FROM turns \
             WHERE stored_at > $1::text::timestamptz \
                OR (stored_at = $1::text::timestamptz AND dedupe_key > $2) \
             ORDER BY stored_at ASC, dedupe_key ASC \
             LIMIT $3"
        );
        let rows = self
            .client
            .query(sql.as_str(), &[&after.at_iso, &after.dedupe_key, &limit])
            .await
            .map_err(MessagesDbError::Query)?;
        Ok(rows.iter().map(row_to_list_item).collect())
    }

    pub async fn get_message(
        &self,
        dedupe_key: &str,
    ) -> Result<Option<MessagesDetail>, MessagesDbError> {
        let sql = format!(
            "SELECT {LIST_COLUMNS}, from_ilk, to_ilk, trace_id, tags, content, message \
             FROM turns WHERE dedupe_key = $1"
        );
        let rows = self
            .client
            .query(sql.as_str(), &[&dedupe_key])
            .await
            .map_err(MessagesDbError::Query)?;
        Ok(rows.first().map(row_to_detail))
    }
}

/// Microseconds in the cursors: a cursor rounded to the millisecond would skip the rows inside it.
/// The sender's name is in the message only when its router stamped it (`src_l2_name`), and a
/// resolved destination is serialized as null: both fall back to the ILKs (`sender_name`,
/// `to_name`).
const LIST_COLUMNS: &str = "dedupe_key, \
     to_char(ts AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS ts_iso, \
     to_char(stored_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') AS stored_at_iso, \
     msg_type, ich, thread_id, \
     message->'routing'->>'src_l2_name' AS src_l2_name, from_ilk AS sender_ilk, \
     COALESCE(message->'routing'->>'dst', to_ilk) AS to_name, \
     octet_length(message::text)::bigint AS size_bytes";

/// Who sent a stored message: the name its router stamped, else (a message copied before routers
/// stamped names) its ILK; never a failed row.
fn sender_name(src_l2_name: Option<String>, sender_ilk: Option<String>) -> String {
    src_l2_name
        .filter(|name| !name.trim().is_empty())
        .or(sender_ilk)
        .unwrap_or_default()
}

fn row_to_list_item(row: &Row) -> MessagesListItem {
    MessagesListItem {
        dedupe_key: row.get("dedupe_key"),
        ts: row.get("ts_iso"),
        stored_at: row.get("stored_at_iso"),
        msg_type: row.get("msg_type"),
        from: sender_name(
            row.try_get("src_l2_name").ok().flatten(),
            row.try_get("sender_ilk").ok().flatten(),
        ),
        to: row.get("to_name"),
        ich: known_ich(row.get("ich")),
        thread_id: row.get("thread_id"),
        size_bytes: row.get("size_bytes"),
    }
}

fn row_to_detail(row: &Row) -> MessagesDetail {
    let item = row_to_list_item(row);
    MessagesDetail {
        dedupe_key: item.dedupe_key,
        ts: item.ts,
        stored_at: item.stored_at,
        msg_type: item.msg_type,
        from: item.from,
        to: item.to,
        from_ilk: row.get("from_ilk"),
        to_ilk: row.get("to_ilk"),
        ich: item.ich,
        thread_id: item.thread_id,
        trace_id: row.get("trace_id"),
        tags: row
            .get::<_, Option<Vec<String>>>("tags")
            .unwrap_or_default(),
        size_bytes: item.size_bytes,
        message: row.get("message"),
        content: row.get("content"),
    }
}

/// SY.storage writes `unknown` when a message carries no ICH.
fn known_ich(ich: String) -> Option<String> {
    let ich = ich.trim();
    (!ich.is_empty() && ich != "unknown").then(|| ich.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every row keeps the whole message (`turns.message` is NOT NULL): the list reads the sender,
    /// the destination and the size from it.
    #[test]
    fn the_list_reads_the_stored_message() {
        assert!(LIST_COLUMNS.contains("message->'routing'->>'src_l2_name' AS src_l2_name"));
        assert!(LIST_COLUMNS.contains("COALESCE(message->'routing'->>'dst', to_ilk) AS to_name"));
        assert!(LIST_COLUMNS.contains("octet_length(message::text)::bigint AS size_bytes"));
        assert!(!LIST_COLUMNS.contains("content"));
    }

    /// A message copied before routers stamped names (the 582 rows backfilled at 0.1.76) has no
    /// `routing.src_l2_name`: its row shows the sender's ILK instead of failing.
    #[test]
    fn a_row_whose_message_has_no_sender_name_shows_the_sender_ilk() {
        let ilk = "ilk:7c1e0a52-1f0b-4a5e-9d64-0b6a1f2d3c4e";
        assert_eq!(sender_name(None, Some(ilk.to_string())), ilk);
        assert_eq!(
            sender_name(Some(" ".to_string()), Some(ilk.to_string())),
            ilk
        );
        assert_eq!(
            sender_name(
                Some("AI.support@motherbee".to_string()),
                Some(ilk.to_string())
            ),
            "AI.support@motherbee"
        );
        assert_eq!(sender_name(None, None), "");
    }
}
