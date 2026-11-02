//! io.slack's thread records (docs/wf-orchestration-v1.md §5.3.4, built as §5.3.6 piece 4).
//!
//! The IO node owns a conversation's thread ids, because it created the communication event. It
//! records the thread id of each Slack place it handles: a place is one channel and one thread in
//! it, or the channel's top level, and has one thread id. A record says who opened its place: a
//! person (a place recorded on the way in, or the `reply_target` of the node's own channel), or the
//! node, with its own post at the top level.
//! - **In:** a reply in a Slack thread whose root is recorded takes the recorded thread id, and the
//!   place keeps its opener. Any other message keeps the id io-common computes for its place, and
//!   its place is recorded under it.
//! - **Out:** a message on a thread id with a place a person opened goes there, and its
//!   `reply_target` is not read. A post the node opened serves one message (the operator,
//!   2026-10-30): the next message on its thread id opens a new post at the top level, whatever its
//!   `reply_target`, and once that post goes out the node forgets its older posts on the thread id.
//!   An unrecorded thread id whose `reply_target` is the node's own channel is posted there, as
//!   before, and recorded. Any other is posted at the top level of the channel, and the post's `ts`
//!   is recorded for its thread id.
//!
//! - **The marks** (docs/wf-orchestration-v1.md §5.4.2, built as §5.4.10 piece 4): a WF marks each
//!   message it sends an IO node with `meta.context.wf.subject` (`Subject`). On a thread id with a
//!   post the node opened, `same` and `end` go into that post's Slack thread, with no new post and
//!   nothing forgotten, so a case keeps one Slack thread; `new`, or no mark, opens a new post as
//!   above. On an unrecorded thread id every mark goes as `new`, and a place a person opened takes
//!   every message, whatever its mark.
//! - **The open menus** (§5.4.4): the node's post that shows a WF's menu as buttons (slack_menu) is
//!   recorded with its channel, its `ts`, the Slack thread it is in, its thread id, the menu and the
//!   post's blocks. A thread id has one open menu per channel: a new one takes the older's place,
//!   and `end` takes it, each to close the older post.
//!
//! So a WF's question, which carries the instance's thread id, is answered on that thread id when a
//! supervisor replies in the Slack thread of its post. A second question on the thread id, days
//! later, is a new post in the channel's view, and a late reply to the first one gets its own
//! thread id, so it never decides the second. And an agent's answer, which carries the
//! `reply_target` of the message it answers (possibly another node's channel), lands where its
//! conversation is on this side.
//!
//! The records are a SQLite file next to the node's `config.json`, because a supervisor may answer
//! hours later, after a restart. A record unused for 30 days is deleted, at start and daily, and so
//! is a menu open for 30 days. When the records fail, the node logs it and works without them: a
//! message keeps the thread id io-common computes, an outbound one goes to the `reply_target` of the
//! node's own channel, or else to the top level, and a menu shows no buttons. Marking a record used
//! is best effort: a record found is used even when that fails.

use anyhow::Result;
use io_common::io_context::{IoContext, SlackPostTarget};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde_json::Value;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The records' file, in the node's own directory, next to its `config.json`.
const THREAD_RECORDS_FILE: &str = "slack_threads.db";
/// A record unused this long is deleted, and a menu open this long.
const RECORD_TTL: Duration = Duration::from_secs(30 * 24 * 60 * 60);
/// How often the unused records are deleted, besides at start.
const EXPIRY_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// One row per Slack place: `thread_ts` is empty for the channel's top level, the times are Unix
/// milliseconds, and `opened_by_node` is 1 for a place the node opened with its own post at the top
/// level, 0 for one a person opened. The index serves the outbound lookup, by thread id.
///
/// One row per open menu (`OpenMenu`): the post that shows it, `channel_id` and `ts`, the Slack
/// thread it is in (`thread_ts`, empty at the top level), the thread id a click goes on, the menu
/// and the post's blocks as JSON, and since when it is open. The unique index keeps one open menu
/// per thread id in a channel. A file from before the menus gains their table when it opens.
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS slack_threads (
    thread_id      TEXT NOT NULL,
    channel_id     TEXT NOT NULL,
    thread_ts      TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    last_used_at   INTEGER NOT NULL,
    opened_by_node INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (channel_id, thread_ts)
);
CREATE INDEX IF NOT EXISTS slack_threads_by_thread_id ON slack_threads (thread_id, channel_id);
CREATE TABLE IF NOT EXISTS slack_menus (
    channel_id TEXT NOT NULL,
    ts         TEXT NOT NULL,
    thread_ts  TEXT NOT NULL,
    thread_id  TEXT NOT NULL,
    menu       TEXT NOT NULL,
    blocks     TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (channel_id, ts)
);
CREATE UNIQUE INDEX IF NOT EXISTS slack_menus_by_thread_id ON slack_menus (thread_id, channel_id);
"#;

/// The columns of an `OpenMenu`, in `OpenMenu::from_row`'s order.
const MENU_COLUMNS: &str = "channel_id, ts, thread_ts, thread_id, menu, blocks";

/// The node's thread records. One connection, shared by the inbound and outbound loops; every call
/// is a short query on a local file and never holds the lock across an `.await`.
pub(crate) struct SlackThreadRecords {
    conn: Mutex<Connection>,
}

/// What the records hold for a thread id in a channel (`SlackThreadRecords::place_of_thread`).
#[derive(Debug, PartialEq, Eq)]
enum ThreadPlaces {
    /// No place.
    Unrecorded,
    /// A place a person opened: its `thread_ts`, empty for the top level.
    PersonOpened(String),
    /// Only posts the node opened, each of which served its one message: the `ts` of the one used
    /// last. A new post forgets the older ones, so there is one.
    NodePosts(String),
}

/// The mark a WF puts on a message it sends an IO node, `meta.context.wf.subject`
/// (docs/wf-orchestration-v1.md §5.4.2): whether the message opens a subject, continues the subject
/// of the WF's last message on its conversation to this node, or ends it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Subject {
    /// `new`, or no mark: as before the marks. A post the node opens serves one message.
    #[default]
    New,
    /// `same`: into the Slack thread of the node's post on the thread id, so a case keeps one
    /// Slack thread.
    Same,
    /// `end`: where `same` puts it, when it has something to post. Then the subject's open menu is
    /// closed (slack_menu).
    End,
}

impl Subject {
    /// The mark in a message's `meta.context`. No mark is `New`, and so is one this node does not
    /// know, with a log line.
    pub(crate) fn of_context(context: Option<&Value>) -> Self {
        let mark = context
            .and_then(|context| context.get("wf"))
            .and_then(|wf| wf.get("subject"));
        match mark {
            None | Some(Value::Null) => Self::New,
            Some(mark) => match mark.as_str() {
                Some("new") => Self::New,
                Some("same") => Self::Same,
                Some("end") => Self::End,
                _ => {
                    tracing::warn!(
                        subject = %mark,
                        "unknown subject mark on an outbound message; it goes as a new subject"
                    );
                    Self::New
                }
            },
        }
    }
}

/// An open menu (docs/wf-orchestration-v1.md §5.4.4): the node's post that shows a WF's menu as
/// buttons, and what a click on it needs (slack_menu).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenMenu {
    /// The post's channel.
    pub(crate) channel_id: String,
    /// The post's `ts`.
    pub(crate) ts: String,
    /// The Slack thread the post is in, empty at the top level: where a click on it is answered.
    pub(crate) thread_ts: String,
    /// The thread id a click goes on: the one of the message that brought the menu.
    pub(crate) thread_id: String,
    /// The menu, as the WF sent it.
    pub(crate) menu: Value,
    /// The post's blocks, which closing the menu posts again without its buttons.
    pub(crate) blocks: Value,
}

impl OpenMenu {
    /// A row of `MENU_COLUMNS`. Its JSON texts are parsed by the caller (`parsed`), so a row that
    /// does not parse is an error of the records, not a panic in the row's mapping.
    fn from_row(row: &Row<'_>) -> rusqlite::Result<(Self, String, String)> {
        Ok((
            Self {
                channel_id: row.get(0)?,
                ts: row.get(1)?,
                thread_ts: row.get(2)?,
                thread_id: row.get(3)?,
                menu: Value::Null,
                blocks: Value::Null,
            },
            row.get(4)?,
            row.get(5)?,
        ))
    }

    fn parsed((mut menu, menu_json, blocks_json): (Self, String, String)) -> Result<Self> {
        menu.menu = serde_json::from_str(&menu_json)?;
        menu.blocks = serde_json::from_str(&blocks_json)?;
        Ok(menu)
    }
}

impl SlackThreadRecords {
    fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::with_connection(conn)
    }

    fn with_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    /// Records in memory, for the tests of the modules that use them.
    #[cfg(test)]
    pub(crate) fn in_memory() -> Result<Self> {
        Self::with_connection(Connection::open_in_memory()?)
    }

    /// A panic elsewhere while the lock was held leaves the connection usable, so the records keep
    /// working rather than fail every call after it.
    fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The thread id recorded for a place, which marks it used (`touch`).
    fn thread_of_place(
        &self,
        channel_id: &str,
        thread_ts: &str,
        now_ms: u64,
    ) -> Result<Option<String>> {
        let conn = self.conn();
        let thread_id: Option<String> = conn
            .query_row(
                "SELECT thread_id FROM slack_threads WHERE channel_id = ?1 AND thread_ts = ?2",
                params![channel_id, thread_ts],
                |row| row.get(0),
            )
            .optional()?;
        if thread_id.is_some() {
            Self::touch(&conn, channel_id, thread_ts, now_ms);
        }
        Ok(thread_id)
    }

    /// What the records hold for a thread id in a channel (`ThreadPlaces`). A place a person opened
    /// is marked used (`touch`), and of two, the thread id goes to the one used last. The node's
    /// own posts are not: a message with no mark opens a new one, and they are forgotten once it
    /// goes out; one that continues the subject marks the post it goes to (`mark_used`).
    fn place_of_thread(
        &self,
        thread_id: &str,
        channel_id: &str,
        now_ms: u64,
    ) -> Result<ThreadPlaces> {
        let conn = self.conn();
        let found: Option<(String, bool)> = conn
            .query_row(
                "SELECT thread_ts, opened_by_node FROM slack_threads \
                 WHERE thread_id = ?1 AND channel_id = ?2 \
                 ORDER BY opened_by_node, last_used_at DESC LIMIT 1",
                params![thread_id, channel_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        Ok(match found {
            None => ThreadPlaces::Unrecorded,
            Some((ts, true)) => ThreadPlaces::NodePosts(ts),
            Some((thread_ts, false)) => {
                Self::touch(&conn, channel_id, &thread_ts, now_ms);
                ThreadPlaces::PersonOpened(thread_ts)
            }
        })
    }

    /// Marks a place used (`touch`): the node's post whose subject a message continues.
    fn mark_used(&self, channel_id: &str, thread_ts: &str, now_ms: u64) {
        Self::touch(&self.conn(), channel_id, thread_ts, now_ms);
    }

    /// Marks a place used, which keeps it from the expiry. Best effort: when it fails (a full disk,
    /// a locked or read-only file) it logs, and the lookup still answers with the record it found,
    /// which is never lost to its mark.
    fn touch(conn: &Connection, channel_id: &str, thread_ts: &str, now_ms: u64) {
        if let Err(err) = conn.execute(
            "UPDATE slack_threads SET last_used_at = ?3 WHERE channel_id = ?1 AND thread_ts = ?2",
            params![channel_id, thread_ts, now_ms],
        ) {
            tracing::warn!(
                channel_id = %channel_id,
                thread_ts = %thread_ts,
                error = %err,
                "slack thread records: marking a record used failed; it is used all the same"
            );
        }
    }

    /// Records a place a person opened under a thread id: one recorded on the way in, or the reply
    /// target of the node's own channel (`upsert`).
    fn record(
        &self,
        thread_id: &str,
        channel_id: &str,
        thread_ts: &str,
        now_ms: u64,
    ) -> Result<()> {
        Self::upsert(
            &self.conn(),
            thread_id,
            channel_id,
            thread_ts,
            false,
            now_ms,
        )
    }

    /// Records the node's own post at the top level, `ts`, as the place it opened for a thread id,
    /// and forgets the older posts it opened for that thread id in the channel, each of which
    /// served its one message: a late reply under one of them gets its own thread id. Both or
    /// neither, and says how many it forgot.
    fn record_node_post(
        &self,
        thread_id: &str,
        channel_id: &str,
        ts: &str,
        now_ms: u64,
    ) -> Result<usize> {
        let mut conn = self.conn();
        let transaction = conn.transaction()?;
        let forgotten = transaction.execute(
            "DELETE FROM slack_threads \
             WHERE thread_id = ?1 AND channel_id = ?2 AND opened_by_node = 1 AND thread_ts <> ?3",
            params![thread_id, channel_id, ts],
        )?;
        Self::upsert(&transaction, thread_id, channel_id, ts, true, now_ms)?;
        transaction.commit()?;
        Ok(forgotten)
    }

    /// Records a place under a thread id, opened by the node or by a person. A place has one thread
    /// id, the last one recorded; when it changes, the record starts again, so `created_at` says
    /// since when the place has this one, and `opened_by_node` who opened it for it.
    fn upsert(
        conn: &Connection,
        thread_id: &str,
        channel_id: &str,
        thread_ts: &str,
        opened_by_node: bool,
        now_ms: u64,
    ) -> Result<()> {
        conn.execute(
            "INSERT INTO slack_threads \
                 (thread_id, channel_id, thread_ts, created_at, last_used_at, opened_by_node) \
             VALUES (?1, ?2, ?3, ?4, ?4, ?5) \
             ON CONFLICT (channel_id, thread_ts) DO UPDATE SET \
                 thread_id = excluded.thread_id, \
                 created_at = CASE WHEN thread_id = excluded.thread_id \
                     THEN created_at ELSE excluded.created_at END, \
                 opened_by_node = CASE WHEN thread_id = excluded.thread_id \
                     THEN opened_by_node ELSE excluded.opened_by_node END, \
                 last_used_at = excluded.last_used_at",
            params![thread_id, channel_id, thread_ts, now_ms, opened_by_node],
        )?;
        Ok(())
    }

    /// Deletes the records unused for [`RECORD_TTL`], and says how many went.
    fn expire(&self, now_ms: u64) -> Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM slack_threads WHERE last_used_at < ?1",
            params![expired_before(now_ms)],
        )?)
    }

    /// Records an open menu, and takes the open menu its thread id had in its channel, which the
    /// new one replaces: the caller closes the older post. Both or neither.
    pub(crate) fn open_menu(&self, menu: &OpenMenu, now_ms: u64) -> Result<Option<OpenMenu>> {
        let mut conn = self.conn();
        let transaction = conn.transaction()?;
        let older = Self::take_menu_where(
            &transaction,
            "thread_id = ?1 AND channel_id = ?2",
            params![menu.thread_id, menu.channel_id],
        )?;
        transaction.execute(
            "INSERT OR REPLACE INTO slack_menus \
                 (channel_id, ts, thread_ts, thread_id, menu, blocks, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                menu.channel_id,
                menu.ts,
                menu.thread_ts,
                menu.thread_id,
                serde_json::to_string(&menu.menu)?,
                serde_json::to_string(&menu.blocks)?,
                now_ms
            ],
        )?;
        transaction.commit()?;
        Ok(older)
    }

    /// The open menu a post shows, if any.
    pub(crate) fn menu_of_post(&self, channel_id: &str, ts: &str) -> Result<Option<OpenMenu>> {
        let conn = self.conn();
        let found = conn
            .query_row(
                &format!(
                    "SELECT {MENU_COLUMNS} FROM slack_menus WHERE channel_id = ?1 AND ts = ?2"
                ),
                params![channel_id, ts],
                OpenMenu::from_row,
            )
            .optional()?;
        found.map(OpenMenu::parsed).transpose()
    }

    /// Takes the open menu of a thread id in a channel, if any: its subject ends, and the caller
    /// closes its post.
    pub(crate) fn take_menu_of_thread(
        &self,
        thread_id: &str,
        channel_id: &str,
    ) -> Result<Option<OpenMenu>> {
        let mut conn = self.conn();
        let transaction = conn.transaction()?;
        let taken = Self::take_menu_where(
            &transaction,
            "thread_id = ?1 AND channel_id = ?2",
            params![thread_id, channel_id],
        )?;
        transaction.commit()?;
        Ok(taken)
    }

    /// Deletes the menu `condition` finds, if any, and returns it.
    fn take_menu_where(
        conn: &Connection,
        condition: &str,
        values: impl rusqlite::Params + Copy,
    ) -> Result<Option<OpenMenu>> {
        let found = conn
            .query_row(
                &format!("SELECT {MENU_COLUMNS} FROM slack_menus WHERE {condition}"),
                values,
                OpenMenu::from_row,
            )
            .optional()?;
        if found.is_some() {
            conn.execute(
                &format!("DELETE FROM slack_menus WHERE {condition}"),
                values,
            )?;
        }
        found.map(OpenMenu::parsed).transpose()
    }

    /// Deletes the menus open for [`RECORD_TTL`], and says how many went. Their posts keep their
    /// buttons, and a click on one is answered as on any closed menu (slack_menu).
    fn expire_menus(&self, now_ms: u64) -> Result<usize> {
        Ok(self.conn().execute(
            "DELETE FROM slack_menus WHERE created_at < ?1",
            params![expired_before(now_ms)],
        )?)
    }
}

/// The time before which a record is past [`RECORD_TTL`].
fn expired_before(now_ms: u64) -> u64 {
    let ttl_ms = u64::try_from(RECORD_TTL.as_millis()).unwrap_or(u64::MAX);
    now_ms.saturating_sub(ttl_ms)
}

/// The node's records, opened in its own directory, or `None` when they cannot open: the node then
/// works without them, and says so here.
pub(crate) fn open_node_thread_records(node_name: &str) -> Option<SlackThreadRecords> {
    match fluxbee_sdk::managed_node_instance_dir(node_name) {
        Ok(node_dir) => open_thread_records(&node_dir, crate::now_epoch_ms()),
        Err(err) => {
            tracing::error!(
                node_name = %node_name,
                error = %err,
                "no node directory for the slack thread records; working without them"
            );
            None
        }
    }
}

/// Opens the records in `node_dir` and deletes the unused ones (the expiry at start).
fn open_thread_records(node_dir: &Path, now_ms: u64) -> Option<SlackThreadRecords> {
    let path = node_dir.join(THREAD_RECORDS_FILE);
    match SlackThreadRecords::open(&path) {
        Ok(records) => {
            tracing::info!(path = %path.display(), "slack thread records open");
            expire_unused(&records, now_ms);
            Some(records)
        }
        Err(err) => {
            tracing::error!(
                path = %path.display(),
                error = %err,
                "cannot open the slack thread records; working without them: a reply in a Slack \
                 thread keeps the thread's own id, and a message goes to its reply_target in this \
                 channel or to the top level"
            );
            None
        }
    }
}

fn expire_unused(records: &SlackThreadRecords, now_ms: u64) {
    match records.expire(now_ms) {
        Ok(0) => {}
        Ok(expired) => {
            tracing::info!(expired, "slack thread records unused for 30 days deleted")
        }
        Err(err) => {
            tracing::warn!(error = %err, "slack thread records: deleting the unused ones failed")
        }
    }
    match records.expire_menus(now_ms) {
        Ok(0) => {}
        Ok(expired) => {
            tracing::info!(expired, "slack menus open for 30 days forgotten")
        }
        Err(err) => {
            tracing::warn!(error = %err, "slack thread records: forgetting the old menus failed")
        }
    }
}

/// Deletes the unused records daily; the open did it at start.
pub(crate) async fn run_thread_records_expiry_loop(records: Arc<SlackThreadRecords>) {
    let mut ticker = tokio::time::interval(EXPIRY_INTERVAL);
    ticker.tick().await; // the first tick is immediate
    loop {
        ticker.tick().await;
        expire_unused(&records, crate::now_epoch_ms());
    }
}

/// In: the thread id of an inbound message. `io` is its IO context as io-common builds it, with the
/// thread id it computes for the message's place, which `meta.thread_id` carries; `thread_ts` is the
/// root of the Slack thread the message is in, if any. A reply under a recorded root takes the
/// recorded thread id; any other message keeps the computed one, and its place is recorded under it.
pub(crate) fn map_inbound_thread(
    records: Option<&SlackThreadRecords>,
    io: &mut IoContext,
    thread_ts: Option<&str>,
    now_ms: u64,
) {
    let Some(records) = records else {
        return;
    };
    let channel_id = io.conversation.id.clone();
    if let Some(root) = thread_ts {
        match records.thread_of_place(&channel_id, root, now_ms) {
            Ok(Some(recorded)) => {
                tracing::debug!(
                    channel_id = %channel_id,
                    thread_ts = %root,
                    thread_id = %recorded,
                    "inbound reply in a recorded Slack thread takes its thread id"
                );
                io.conversation.thread_id = Some(recorded);
                return;
            }
            Ok(None) => {}
            Err(err) => {
                // Whether the place is recorded is unknown: the message keeps the computed id, and
                // nothing is recorded that could replace the right one.
                tracing::warn!(
                    channel_id = %channel_id,
                    thread_ts = %root,
                    error = %err,
                    "slack thread records: lookup failed; the message keeps its own thread id"
                );
                return;
            }
        }
    }
    let Some(thread_id) = io.conversation.thread_id.as_deref() else {
        return;
    };
    if let Err(err) = records.record(thread_id, &channel_id, thread_ts.unwrap_or(""), now_ms) {
        tracing::warn!(
            channel_id = %channel_id,
            thread_ts = thread_ts.unwrap_or(""),
            thread_id = %thread_id,
            error = %err,
            "slack thread records: recording an inbound place failed"
        );
    }
}

/// Where an outbound message's posts go in the node's channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OutboundPlace {
    /// Its thread id is recorded at a place a person opened, or it continues the subject of the
    /// node's post on its thread id, or the message's first post opened its place: that thread,
    /// `None` for the top level.
    Recorded(Option<String>),
    /// Unrecorded, with a `reply_target` of the node's own channel: that thread, `None` for the top
    /// level, as before the records.
    ReplyTarget(Option<String>),
    /// A new post at the top level, which the node opens: for a thread id it has only its own
    /// posts for, or for anything else.
    NewPost,
}

/// One outbound message on its way through the records: where its posts go, and the record the
/// first post that can makes.
pub(crate) struct OutboundThread<'a> {
    records: Option<&'a SlackThreadRecords>,
    channel_id: &'a str,
    /// The thread id a post records its place under; `None` once there is nothing to record.
    record_as: Option<&'a str>,
    place: OutboundPlace,
}

impl<'a> OutboundThread<'a> {
    /// Out: `workspace_id` and `channel_id` are the node's binding, `thread_id` the message's
    /// `meta.thread_id`, `reply_target` its `meta.context.io.reply_target` and `subject` its mark.
    /// A `reply_target` of another channel, or another workspace, is never read, and on a recorded
    /// thread id none is. The mark matters only on a thread id the node opened its post for: `same`
    /// and `end` go into that post's thread.
    pub(crate) fn resolve(
        records: Option<&'a SlackThreadRecords>,
        workspace_id: &str,
        channel_id: &'a str,
        thread_id: Option<&'a str>,
        reply_target: Option<&SlackPostTarget>,
        subject: Subject,
        now_ms: u64,
    ) -> Self {
        let thread_id = thread_id.map(str::trim).filter(|id| !id.is_empty());
        let mut record_as = thread_id;
        if let (Some(records), Some(thread_id)) = (records, thread_id) {
            match records.place_of_thread(thread_id, channel_id, now_ms) {
                Ok(ThreadPlaces::PersonOpened(thread_ts)) => {
                    let place = OutboundPlace::Recorded(non_empty(thread_ts));
                    tracing::debug!(
                        channel_id = %channel_id,
                        thread_id = %thread_id,
                        place = ?place,
                        "outbound message on a Slack thread a person opened"
                    );
                    return Self {
                        records: Some(records),
                        channel_id,
                        record_as: None,
                        place,
                    };
                }
                Ok(ThreadPlaces::NodePosts(post_ts)) if subject != Subject::New => {
                    // The message continues, or ends, the subject of the node's post: into its
                    // thread, with no new post and nothing forgotten, and the post is used.
                    records.mark_used(channel_id, &post_ts, now_ms);
                    let place = OutboundPlace::Recorded(Some(post_ts));
                    tracing::debug!(
                        channel_id = %channel_id,
                        thread_id = %thread_id,
                        subject = ?subject,
                        place = ?place,
                        "outbound message on the subject of the node's post on this thread id"
                    );
                    return Self {
                        records: Some(records),
                        channel_id,
                        record_as: None,
                        place,
                    };
                }
                Ok(ThreadPlaces::NodePosts(_)) => {
                    // Each of the node's posts served its one message: this one opens a new post,
                    // whatever reply target it carries, and `posted` forgets the older ones.
                    tracing::debug!(
                        channel_id = %channel_id,
                        thread_id = %thread_id,
                        "outbound message on a thread id the node opened its posts for: a new post"
                    );
                    return Self {
                        records: Some(records),
                        channel_id,
                        record_as: Some(thread_id),
                        place: OutboundPlace::NewPost,
                    };
                }
                Ok(ThreadPlaces::Unrecorded) => {}
                Err(err) => {
                    // As with the records closed: the reply_target of this channel or the top
                    // level, and nothing recorded.
                    tracing::warn!(
                        channel_id = %channel_id,
                        thread_id = %thread_id,
                        error = %err,
                        "slack thread records: lookup failed; the message goes out unrecorded"
                    );
                    record_as = None;
                }
            }
        }
        let own_target = reply_target.filter(|target| {
            target.channel_id == channel_id
                && target
                    .workspace_id
                    .as_deref()
                    .is_none_or(|workspace| workspace == workspace_id)
        });
        let place = match own_target {
            Some(target) => {
                OutboundPlace::ReplyTarget(target.thread_ts.clone().and_then(non_empty))
            }
            None => OutboundPlace::NewPost,
        };
        let reply_target_channel = reply_target.map(|target| target.channel_id.as_str());
        tracing::debug!(
            channel_id = %channel_id,
            thread_id = thread_id.unwrap_or(""),
            reply_target_channel = reply_target_channel.unwrap_or(""),
            place = ?place,
            "outbound message on an unrecorded Slack thread"
        );
        Self {
            records,
            channel_id,
            record_as,
            place,
        }
    }

    /// The thread the message's next post goes to; `None` is the channel's top level.
    pub(crate) fn thread_ts(&self) -> Option<&str> {
        match &self.place {
            OutboundPlace::Recorded(thread_ts) | OutboundPlace::ReplyTarget(thread_ts) => {
                thread_ts.as_deref()
            }
            OutboundPlace::NewPost => None,
        }
    }

    /// A post of the message went out; `ts` is the new Slack message's, when Slack answers one (a
    /// file upload does not). The first post that can records the place for the thread id, and the
    /// message's other posts go there: after a new post, into its thread. A new post is recorded
    /// as the node's, and the node's older posts on the thread id are forgotten
    /// (`SlackThreadRecords::record_node_post`).
    pub(crate) fn posted(&mut self, ts: Option<&str>, now_ms: u64) {
        let (Some(records), Some(thread_id)) = (self.records, self.record_as) else {
            return;
        };
        let (thread_ts, opened_by_node) = match &self.place {
            OutboundPlace::Recorded(_) => return,
            OutboundPlace::ReplyTarget(thread_ts) => (thread_ts.clone(), false),
            OutboundPlace::NewPost => match ts {
                Some(ts) => (Some(ts.to_string()), true),
                None => return,
            },
        };
        self.record_as = None;
        let place = thread_ts.as_deref().unwrap_or("");
        let recorded = if opened_by_node {
            records.record_node_post(thread_id, self.channel_id, place, now_ms)
        } else {
            records
                .record(thread_id, self.channel_id, place, now_ms)
                .map(|()| 0)
        };
        match recorded {
            Ok(forgotten) => {
                tracing::debug!(
                    channel_id = %self.channel_id,
                    thread_ts = %place,
                    thread_id = %thread_id,
                    opened_by_node,
                    "slack thread recorded for an outbound message"
                );
                if forgotten > 0 {
                    tracing::info!(
                        channel_id = %self.channel_id,
                        thread_id = %thread_id,
                        forgotten,
                        "the node's older posts on this thread id are forgotten: a late reply \
                         under one of them gets its own thread id"
                    );
                }
                self.place = OutboundPlace::Recorded(thread_ts);
            }
            Err(err) => {
                tracing::warn!(
                    channel_id = %self.channel_id,
                    thread_id = %thread_id,
                    error = %err,
                    "slack thread records: recording an outbound place failed"
                );
            }
        }
    }
}

fn non_empty(value: String) -> Option<String> {
    Some(value).filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use io_common::io_context::{slack_inbound_io_context, wrap_in_meta_context};
    use io_common::router_message::{build_user_message, DEFAULT_TTL};
    use serde_json::json;
    use std::path::PathBuf;

    const WORKSPACE: &str = "T0WORKSPACE";
    const CHANNEL: &str = "C0SUPERVISORS";
    const NOW: u64 = 1_793_000_000_000;
    const DAY_MS: u64 = 24 * 60 * 60 * 1000;
    /// The thread id a WF instance puts on every message it sends.
    const WF_THREAD: &str = "thread:sha256:wf-instance";
    /// The thread id of a conversation a person started in a Slack thread.
    const PERSON_THREAD: &str = "thread:sha256:person-thread";

    fn records() -> SlackThreadRecords {
        SlackThreadRecords::with_connection(Connection::open_in_memory().expect("in-memory db"))
            .expect("schema")
    }

    /// A message in the bound channel: in the Slack thread `thread_ts`, or at the top level. Its
    /// IO context carries the thread id io-common computes for that place.
    fn inbound(thread_ts: Option<&str>) -> IoContext {
        slack_inbound_io_context(WORKSPACE, "U0SUPERVISOR", CHANNEL, thread_ts, "Ev0001")
    }

    fn target(channel_id: &str, thread_ts: Option<&str>) -> SlackPostTarget {
        SlackPostTarget {
            channel_id: channel_id.to_string(),
            thread_ts: thread_ts.map(str::to_string),
            workspace_id: Some(WORKSPACE.to_string()),
        }
    }

    fn resolve<'a>(
        records: Option<&'a SlackThreadRecords>,
        thread_id: Option<&'a str>,
        reply_target: Option<&SlackPostTarget>,
    ) -> OutboundThread<'a> {
        resolve_marked(records, thread_id, reply_target, Subject::New)
    }

    fn resolve_marked<'a>(
        records: Option<&'a SlackThreadRecords>,
        thread_id: Option<&'a str>,
        reply_target: Option<&SlackPostTarget>,
        subject: Subject,
    ) -> OutboundThread<'a> {
        OutboundThread::resolve(
            records,
            WORKSPACE,
            CHANNEL,
            thread_id,
            reply_target,
            subject,
            NOW,
        )
    }

    /// A record as (thread_id, channel_id, thread_ts, created_at, last_used_at, opened_by_node).
    type Row = (String, String, String, u64, u64, bool);

    /// Every record.
    fn rows(records: &SlackThreadRecords) -> Vec<Row> {
        let conn = records.conn();
        let mut statement = conn
            .prepare(
                "SELECT thread_id, channel_id, thread_ts, created_at, last_used_at, opened_by_node \
                 FROM slack_threads ORDER BY channel_id, thread_ts",
            )
            .expect("prepare");
        let mapped = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            })
            .expect("query");
        mapped.collect::<rusqlite::Result<Vec<_>>>().expect("rows")
    }

    /// A record of a place a person opened, in the bound channel.
    fn row(thread_id: &str, thread_ts: &str, created_at: u64, last_used_at: u64) -> Row {
        (
            thread_id.to_string(),
            CHANNEL.to_string(),
            thread_ts.to_string(),
            created_at,
            last_used_at,
            false,
        )
    }

    /// A record of a post the node opened in the bound channel.
    fn node_row(thread_id: &str, ts: &str, created_at: u64, last_used_at: u64) -> Row {
        let mut record = row(thread_id, ts, created_at, last_used_at);
        record.5 = true;
        record
    }

    /// The WF's message `thread_id` posted at the top level, as the outbound loop posts it: Slack
    /// answers the post's `ts`.
    fn post_at_top_level(records: &SlackThreadRecords, thread_id: &str, ts: &str, now_ms: u64) {
        let mut thread = OutboundThread::resolve(
            Some(records),
            WORKSPACE,
            CHANNEL,
            Some(thread_id),
            None,
            Subject::New,
            now_ms,
        );
        assert_eq!(thread.place, OutboundPlace::NewPost, "{thread_id}");
        thread.posted(Some(ts), now_ms);
    }

    fn temp_node_dir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("io-slack-threads-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("node dir");
        dir
    }

    /// The way back of a consultation: the WF's question was posted at the top level and its `ts`
    /// recorded for the instance's thread id. A supervisor's reply in that post's Slack thread
    /// takes it, in the IO context and so in `meta.thread_id`, and reaches the instance.
    #[test]
    fn a_reply_under_a_recorded_post_takes_its_thread_id() {
        let records = records();
        post_at_top_level(&records, WF_THREAD, "1793000000.000100", NOW - DAY_MS);

        let mut io = inbound(Some("1793000000.000100"));
        let native_thread = io.conversation.thread_id.clone().expect("computed id");
        map_inbound_thread(Some(&records), &mut io, Some("1793000000.000100"), NOW);

        assert_ne!(native_thread, WF_THREAD);
        assert_eq!(io.conversation.thread_id.as_deref(), Some(WF_THREAD));
        let msg = build_user_message(
            "uuid",
            None,
            DEFAULT_TTL,
            "trace".to_string(),
            None,
            None,
            wrap_in_meta_context(&io),
            json!({"type": "text", "content": "approved"}),
        );
        assert_eq!(msg.meta.thread_id.as_deref(), Some(WF_THREAD));
        // The reply target still names the Slack thread, and the reply was a use of the record,
        // which stays the node's post.
        assert_eq!(
            io.reply_target.params["thread_ts"].as_str(),
            Some("1793000000.000100")
        );
        assert_eq!(
            rows(&records),
            vec![node_row(WF_THREAD, "1793000000.000100", NOW - DAY_MS, NOW)]
        );
    }

    /// Any other message keeps the thread id io-common computes, and its place is recorded under
    /// it: a message at the top level, and a reply in a Slack thread nothing recorded. An answer on
    /// those ids then goes back where each came from.
    #[test]
    fn other_inbound_messages_keep_their_thread_id_and_record_their_place() {
        let records = records();
        // Only a reply in a Slack thread reads the records: a message at the top level keeps its
        // own id even when the top level was recorded under another (an outbound message posted
        // there), and takes the place back.
        records
            .record("thread:sha256:posted-here", CHANNEL, "", NOW - DAY_MS)
            .expect("record");

        let mut top = inbound(None);
        let channel_thread = top.conversation.thread_id.clone().expect("computed id");
        map_inbound_thread(Some(&records), &mut top, None, NOW);
        assert_eq!(
            top.conversation.thread_id.as_deref(),
            Some(channel_thread.as_str())
        );

        let mut reply = inbound(Some("1793000000.000200"));
        let native_thread = reply.conversation.thread_id.clone().expect("computed id");
        map_inbound_thread(Some(&records), &mut reply, Some("1793000000.000200"), NOW);
        assert_eq!(
            reply.conversation.thread_id.as_deref(),
            Some(native_thread.as_str())
        );

        assert_eq!(
            rows(&records),
            vec![
                row(&channel_thread, "", NOW, NOW),
                row(&native_thread, "1793000000.000200", NOW, NOW),
            ]
        );
        assert_eq!(
            resolve(Some(&records), Some(&channel_thread), None).place,
            OutboundPlace::Recorded(None)
        );
        assert_eq!(
            resolve(Some(&records), Some(&native_thread), None).place,
            OutboundPlace::Recorded(Some("1793000000.000200".to_string()))
        );
    }

    /// A message on a thread id recorded at a place a person opened goes to that place; its reply
    /// target is not read, even one of this channel, and its posts record nothing more.
    #[test]
    fn a_recorded_thread_goes_to_its_place_and_its_reply_target_is_not_read() {
        let records = records();
        records
            .record(PERSON_THREAD, CHANNEL, "1793000000.000300", NOW - DAY_MS)
            .expect("record");
        let elsewhere_here = target(CHANNEL, Some("1793000000.000999"));

        let mut thread = resolve(Some(&records), Some(PERSON_THREAD), Some(&elsewhere_here));
        assert_eq!(
            thread.place,
            OutboundPlace::Recorded(Some("1793000000.000300".to_string()))
        );
        assert_eq!(thread.thread_ts(), Some("1793000000.000300"));
        thread.posted(Some("1793000000.000301"), NOW);
        assert_eq!(thread.thread_ts(), Some("1793000000.000300"));
        assert_eq!(
            rows(&records),
            vec![row(PERSON_THREAD, "1793000000.000300", NOW - DAY_MS, NOW)]
        );

        // A record of the top level posts there.
        records
            .record("thread:sha256:top", CHANNEL, "", NOW)
            .expect("record");
        let top = resolve(
            Some(&records),
            Some("thread:sha256:top"),
            Some(&elsewhere_here),
        );
        assert_eq!(top.thread_ts(), None);

        // A record of another channel (a binding the node had before) is not this channel's.
        records
            .record(
                "thread:sha256:old",
                "C0OLDCHANNEL",
                "1793000000.000302",
                NOW,
            )
            .expect("record");
        assert_eq!(
            resolve(Some(&records), Some("thread:sha256:old"), None).place,
            OutboundPlace::NewPost
        );
    }

    /// The bug this closes: an agent's answer carries the reply target of the message it answers,
    /// so one forwarded here can name another channel. It was posted in this channel with the
    /// other's `thread_ts`, a thread that does not exist here.
    #[test]
    fn a_reply_target_of_another_channel_is_ignored() {
        let records = records();
        let foreign = target("C0SALES", Some("1793000000.000400"));

        let thread = resolve(Some(&records), Some("thread:sha256:sales"), Some(&foreign));
        assert_eq!(thread.place, OutboundPlace::NewPost);
        assert_eq!(thread.thread_ts(), None);

        // This channel's id in another workspace is not this channel either.
        let other_workspace = SlackPostTarget {
            workspace_id: Some("T0OTHER".to_string()),
            ..target(CHANNEL, Some("1793000000.000401"))
        };
        assert_eq!(
            resolve(
                Some(&records),
                Some("thread:sha256:sales"),
                Some(&other_workspace)
            )
            .place,
            OutboundPlace::NewPost
        );

        // On a recorded thread id, the record decides.
        records
            .record("thread:sha256:person", CHANNEL, "", NOW)
            .expect("record");
        assert_eq!(
            resolve(Some(&records), Some("thread:sha256:person"), Some(&foreign)).place,
            OutboundPlace::Recorded(None)
        );
    }

    /// The WF's question: its thread id is the instance's, unknown here, and it brings no reply
    /// target. It is posted at the top level, the post's `ts` is recorded as a place the node
    /// opened, and the rest of the message follows into the post's Slack thread. A reply there
    /// reaches the thread id.
    #[test]
    fn an_unrecorded_thread_with_no_target_is_posted_at_the_top_level_then_recorded() {
        let records = records();
        let mut thread = resolve(Some(&records), Some(WF_THREAD), None);
        assert_eq!(thread.place, OutboundPlace::NewPost);
        assert_eq!(thread.thread_ts(), None);

        // A file upload answers no ts, so nothing is recorded yet.
        thread.posted(None, NOW);
        assert!(rows(&records).is_empty());
        assert_eq!(thread.thread_ts(), None);

        thread.posted(Some("1793000000.000500"), NOW);
        assert_eq!(thread.thread_ts(), Some("1793000000.000500"));
        assert_eq!(
            rows(&records),
            vec![node_row(WF_THREAD, "1793000000.000500", NOW, NOW)]
        );
        let mut reply = inbound(Some("1793000000.000500"));
        map_inbound_thread(Some(&records), &mut reply, Some("1793000000.000500"), NOW);
        assert_eq!(reply.conversation.thread_id.as_deref(), Some(WF_THREAD));

        // A message without a thread id has nothing to record: all of it stays at the top level.
        let mut no_thread = resolve(Some(&records), None, None);
        no_thread.posted(Some("1793000000.000501"), NOW);
        assert_eq!(no_thread.thread_ts(), None);
        assert_eq!(rows(&records).len(), 1);
    }

    /// An unrecorded thread id whose reply target is this channel is posted there, as before the
    /// records, and recorded: any post is enough, since the place is known.
    #[test]
    fn an_unrecorded_thread_with_a_target_in_this_channel_is_posted_there_and_recorded() {
        let records = records();
        let here = target(CHANNEL, Some("1793000000.000600"));

        let mut thread = resolve(Some(&records), Some("thread:sha256:agent"), Some(&here));
        assert_eq!(
            thread.place,
            OutboundPlace::ReplyTarget(Some("1793000000.000600".to_string()))
        );
        assert_eq!(thread.thread_ts(), Some("1793000000.000600"));
        thread.posted(None, NOW);
        assert_eq!(
            rows(&records),
            vec![row("thread:sha256:agent", "1793000000.000600", NOW, NOW)]
        );
        assert_eq!(
            resolve(Some(&records), Some("thread:sha256:agent"), None).place,
            OutboundPlace::Recorded(Some("1793000000.000600".to_string()))
        );

        // A target with no workspace is read by its channel.
        let bare = SlackPostTarget {
            workspace_id: None,
            ..target(CHANNEL, None)
        };
        assert_eq!(
            resolve(Some(&records), None, Some(&bare)).place,
            OutboundPlace::ReplyTarget(None)
        );
    }

    /// A place has one thread id, the last one recorded; `created_at` says since when.
    #[test]
    fn a_place_has_one_thread_id_the_last_recorded() {
        let records = records();
        records
            .record("thread:sha256:first", CHANNEL, "", NOW - DAY_MS)
            .expect("record");
        records
            .record("thread:sha256:first", CHANNEL, "", NOW)
            .expect("record");
        assert_eq!(
            rows(&records),
            vec![row("thread:sha256:first", "", NOW - DAY_MS, NOW)]
        );

        records
            .record("thread:sha256:second", CHANNEL, "", NOW + 1)
            .expect("record");
        assert_eq!(
            rows(&records),
            vec![row("thread:sha256:second", "", NOW + 1, NOW + 1)]
        );
        assert_eq!(
            resolve(Some(&records), Some("thread:sha256:first"), None).place,
            OutboundPlace::NewPost
        );
    }

    /// A thread id that came to have two places a person opened goes to the one used last.
    #[test]
    fn a_thread_id_with_two_places_goes_to_the_one_used_last() {
        let records = records();
        records
            .record(
                PERSON_THREAD,
                CHANNEL,
                "1793000000.000700",
                NOW - 2 * DAY_MS,
            )
            .expect("record");
        records
            .record(PERSON_THREAD, CHANNEL, "1793000000.000701", NOW - DAY_MS)
            .expect("record");
        assert_eq!(
            resolve(Some(&records), Some(PERSON_THREAD), None).thread_ts(),
            Some("1793000000.000701")
        );

        let mut reply = inbound(Some("1793000000.000700"));
        map_inbound_thread(
            Some(&records),
            &mut reply,
            Some("1793000000.000700"),
            NOW + 1,
        );
        assert_eq!(
            resolve(Some(&records), Some(PERSON_THREAD), None).thread_ts(),
            Some("1793000000.000700")
        );
    }

    /// The records outlive a restart, and a record unused for 30 days is deleted when the node
    /// opens them and by the daily pass. A use keeps a record.
    #[test]
    fn records_unused_for_30_days_are_deleted_at_start_and_daily() {
        let dir = temp_node_dir("expiry");
        {
            let records = open_thread_records(&dir, NOW - 40 * DAY_MS).expect("open");
            for (thread_id, thread_ts) in [
                ("thread:sha256:unused", "1793000000.000801"),
                ("thread:sha256:used", "1793000000.000802"),
            ] {
                records
                    .record(thread_id, CHANNEL, thread_ts, NOW - 31 * DAY_MS)
                    .expect("record");
            }
            records
                .record(
                    "thread:sha256:recent",
                    CHANNEL,
                    "1793000000.000803",
                    NOW - 29 * DAY_MS,
                )
                .expect("record");
            let used = OutboundThread::resolve(
                Some(&records),
                WORKSPACE,
                CHANNEL,
                Some("thread:sha256:used"),
                None,
                Subject::New,
                NOW - 2 * DAY_MS,
            );
            assert_eq!(used.thread_ts(), Some("1793000000.000802"));
        }

        // At start.
        let records = open_thread_records(&dir, NOW).expect("reopen");
        assert_eq!(
            rows(&records),
            vec![
                row(
                    "thread:sha256:used",
                    "1793000000.000802",
                    NOW - 31 * DAY_MS,
                    NOW - 2 * DAY_MS
                ),
                row(
                    "thread:sha256:recent",
                    "1793000000.000803",
                    NOW - 29 * DAY_MS,
                    NOW - 29 * DAY_MS
                ),
            ]
        );
        assert_eq!(
            resolve(Some(&records), Some("thread:sha256:used"), None).thread_ts(),
            Some("1793000000.000802")
        );

        // The daily pass, two days on: the recent one is past 30 days unused too.
        assert_eq!(records.expire(NOW + 2 * DAY_MS).expect("expire"), 1);
        assert_eq!(
            rows(&records)
                .into_iter()
                .map(|row| row.0)
                .collect::<Vec<_>>(),
            vec!["thread:sha256:used".to_string()]
        );

        drop(records);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The records failing never stops the node: it logs, and works as before them. A message
    /// keeps its own thread id, and one going out goes to this channel's reply target or to the
    /// top level, all of it, with nothing recorded.
    #[test]
    fn when_the_records_fail_the_node_works_as_before_them() {
        let here = target(CHANNEL, Some("1793000000.000900"));

        // They cannot open: their directory does not exist.
        let missing = std::env::temp_dir()
            .join(format!("io-slack-threads-missing-{}", std::process::id()))
            .join("node");
        assert!(open_thread_records(&missing, NOW).is_none());
        let mut reply = inbound(Some("1793000000.000900"));
        let native_thread = reply.conversation.thread_id.clone();
        map_inbound_thread(None, &mut reply, Some("1793000000.000900"), NOW);
        assert_eq!(reply.conversation.thread_id, native_thread);
        assert_eq!(
            resolve(None, Some(WF_THREAD), Some(&here)).place,
            OutboundPlace::ReplyTarget(Some("1793000000.000900".to_string()))
        );
        let mut new_post = resolve(None, Some(WF_THREAD), None);
        assert_eq!(new_post.place, OutboundPlace::NewPost);
        new_post.posted(Some("1793000000.000901"), NOW);
        assert_eq!(new_post.thread_ts(), None);

        // Their queries fail: the table is gone.
        let records = records();
        records
            .record(WF_THREAD, CHANNEL, "1793000000.000902", NOW)
            .expect("record");
        records
            .conn()
            .execute_batch("DROP TABLE slack_threads")
            .expect("drop");
        let mut reply = inbound(Some("1793000000.000902"));
        let native_thread = reply.conversation.thread_id.clone();
        map_inbound_thread(Some(&records), &mut reply, Some("1793000000.000902"), NOW);
        assert_eq!(reply.conversation.thread_id, native_thread);
        let mut top = inbound(None);
        let channel_thread = top.conversation.thread_id.clone();
        map_inbound_thread(Some(&records), &mut top, None, NOW);
        assert_eq!(top.conversation.thread_id, channel_thread);

        let mut thread = resolve(Some(&records), Some(WF_THREAD), Some(&here));
        assert_eq!(
            thread.place,
            OutboundPlace::ReplyTarget(Some("1793000000.000900".to_string()))
        );
        thread.posted(None, NOW);
        assert_eq!(thread.record_as, None);
        let mut new_post = resolve(Some(&records), Some(WF_THREAD), None);
        assert_eq!(new_post.place, OutboundPlace::NewPost);
        new_post.posted(Some("1793000000.000903"), NOW);
        assert_eq!(new_post.thread_ts(), None);
        assert!(records.expire(NOW).is_err());
    }

    /// Recording the node's new post fails, and its transaction is undone: the post went out, the
    /// rest of the message stays at the top level, as when the records fail, and the records keep
    /// what they had.
    #[test]
    fn when_recording_the_nodes_post_fails_the_records_keep_what_they_had() {
        let records = records();
        post_at_top_level(&records, WF_THREAD, "1793000000.000904", NOW - DAY_MS);
        records
            .conn()
            .execute_batch(
                "CREATE TRIGGER record_fails BEFORE INSERT ON slack_threads \
                 BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;",
            )
            .expect("trigger");
        let mut second = resolve(Some(&records), Some(WF_THREAD), None);
        assert_eq!(second.place, OutboundPlace::NewPost);
        second.posted(Some("1793000000.000905"), NOW);
        assert_eq!(second.thread_ts(), None);
        assert_eq!(
            rows(&records),
            vec![node_row(
                WF_THREAD,
                "1793000000.000904",
                NOW - DAY_MS,
                NOW - DAY_MS
            )]
        );
    }

    /// Marking a record used is best effort: when that fails (a full disk, a locked or read-only
    /// file), the record found still decides. A supervisor's reply under the WF's post still
    /// reaches the WF's instance, and a message on a thread a person opened still goes to its
    /// place, not to a new post at the top level.
    #[test]
    fn a_record_found_is_used_even_when_marking_it_used_fails() {
        let records = records();
        post_at_top_level(&records, WF_THREAD, "1793000000.001000", NOW - DAY_MS);
        records
            .record(PERSON_THREAD, CHANNEL, "1793000000.001001", NOW - DAY_MS)
            .expect("record");
        records
            .conn()
            .execute_batch(
                "CREATE TRIGGER touch_fails BEFORE UPDATE ON slack_threads \
                 BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END;",
            )
            .expect("trigger");

        let mut reply = inbound(Some("1793000000.001000"));
        map_inbound_thread(Some(&records), &mut reply, Some("1793000000.001000"), NOW);
        assert_eq!(reply.conversation.thread_id.as_deref(), Some(WF_THREAD));

        let thread = resolve(Some(&records), Some(PERSON_THREAD), None);
        assert_eq!(
            thread.place,
            OutboundPlace::Recorded(Some("1793000000.001001".to_string()))
        );
        assert_eq!(thread.thread_ts(), Some("1793000000.001001"));

        // Neither use was marked.
        assert_eq!(
            rows(&records),
            vec![
                node_row(WF_THREAD, "1793000000.001000", NOW - DAY_MS, NOW - DAY_MS),
                row(
                    PERSON_THREAD,
                    "1793000000.001001",
                    NOW - DAY_MS,
                    NOW - DAY_MS
                ),
            ]
        );
    }

    /// The node's own post serves one message (docs/wf-orchestration-v1.md §5.3.4, decided after
    /// the review). The next message on its thread id, a second consultation days later, opens a
    /// new post at the top level, in the channel's view, whatever reply target it carries, and the
    /// node forgets its first post. A late reply under the first post, meant for the first
    /// question, keeps its own thread id and never decides the second; a reply under the new post
    /// reaches the thread id, and leaves the post the node's.
    #[test]
    fn a_second_message_on_a_thread_opens_a_new_post_and_forgets_the_first() {
        let records = records();
        post_at_top_level(&records, WF_THREAD, "1793000000.001100", NOW - 3 * DAY_MS);
        let mut answer = inbound(Some("1793000000.001100"));
        map_inbound_thread(
            Some(&records),
            &mut answer,
            Some("1793000000.001100"),
            NOW - 2 * DAY_MS,
        );
        assert_eq!(answer.conversation.thread_id.as_deref(), Some(WF_THREAD));

        // The second message carries the reply target of the supervisor's answer under the first
        // post, as an agent's answer to it would.
        let under_first = target(CHANNEL, Some("1793000000.001100"));
        let mut second = resolve(Some(&records), Some(WF_THREAD), Some(&under_first));
        assert_eq!(second.place, OutboundPlace::NewPost);
        assert_eq!(second.thread_ts(), None);
        // A file upload answers no ts: nothing changes yet.
        second.posted(None, NOW);
        assert_eq!(second.thread_ts(), None);
        assert_eq!(rows(&records).len(), 1);
        // The post: recorded as the node's, and the rest of the message goes into its thread.
        second.posted(Some("1793000000.001200"), NOW);
        assert_eq!(second.thread_ts(), Some("1793000000.001200"));
        assert_eq!(
            rows(&records),
            vec![node_row(WF_THREAD, "1793000000.001200", NOW, NOW)]
        );

        // A late reply under the first post keeps its computed id, and its place is a person's.
        let mut late = inbound(Some("1793000000.001100"));
        let late_thread = late.conversation.thread_id.clone().expect("computed id");
        map_inbound_thread(
            Some(&records),
            &mut late,
            Some("1793000000.001100"),
            NOW + 1,
        );
        assert_ne!(late_thread, WF_THREAD);
        assert_eq!(
            late.conversation.thread_id.as_deref(),
            Some(late_thread.as_str())
        );

        // A reply under the new post reaches the thread id; the post stays the node's, so the next
        // message on the thread id opens a post of its own again.
        let mut reply = inbound(Some("1793000000.001200"));
        map_inbound_thread(
            Some(&records),
            &mut reply,
            Some("1793000000.001200"),
            NOW + 2,
        );
        assert_eq!(reply.conversation.thread_id.as_deref(), Some(WF_THREAD));
        assert_eq!(
            rows(&records),
            vec![
                row(&late_thread, "1793000000.001100", NOW + 1, NOW + 1),
                node_row(WF_THREAD, "1793000000.001200", NOW, NOW + 2),
            ]
        );
        assert_eq!(
            resolve(Some(&records), Some(WF_THREAD), None).place,
            OutboundPlace::NewPost
        );
    }

    /// A place a person opened keeps today's behavior: each message on its thread id goes there,
    /// and nothing more is recorded. It wins over a post of the node's on the same thread id.
    #[test]
    fn a_place_a_person_opened_keeps_its_messages() {
        let records = records();
        let mut person = inbound(Some("1793000000.001300"));
        let person_thread = person.conversation.thread_id.clone().expect("computed id");
        map_inbound_thread(
            Some(&records),
            &mut person,
            Some("1793000000.001300"),
            NOW - DAY_MS,
        );
        for at in [NOW, NOW + 1] {
            let mut answer = OutboundThread::resolve(
                Some(&records),
                WORKSPACE,
                CHANNEL,
                Some(person_thread.as_str()),
                None,
                Subject::New,
                at,
            );
            assert_eq!(
                answer.place,
                OutboundPlace::Recorded(Some("1793000000.001300".to_string()))
            );
            answer.posted(Some("1793000000.001399"), at);
            assert_eq!(answer.thread_ts(), Some("1793000000.001300"));
        }
        assert_eq!(
            rows(&records),
            vec![row(
                &person_thread,
                "1793000000.001300",
                NOW - DAY_MS,
                NOW + 1
            )]
        );

        records
            .record_node_post(&person_thread, CHANNEL, "1793000000.001301", NOW + 2)
            .expect("record");
        assert_eq!(
            resolve(Some(&records), Some(person_thread.as_str()), None).place,
            OutboundPlace::Recorded(Some("1793000000.001300".to_string()))
        );
    }

    /// The marks (docs/wf-orchestration-v1.md §5.4.2): on a thread id with the node's post, `same`
    /// and `end` go into the post's Slack thread, with no new post and nothing forgotten, whatever
    /// reply target they carry, and the post is used. A case keeps one Slack thread: a reply there
    /// still reaches the thread id. `new` opens a new post, as before, which `same` follows next.
    #[test]
    fn same_and_end_go_into_the_thread_of_the_nodes_post() {
        let records = records();
        post_at_top_level(&records, WF_THREAD, "1793000000.001400", NOW - DAY_MS);
        let elsewhere_here = target(CHANNEL, Some("1793000000.000999"));

        for subject in [Subject::Same, Subject::End] {
            for reply_target in [None, Some(&elsewhere_here)] {
                let mut thread =
                    resolve_marked(Some(&records), Some(WF_THREAD), reply_target, subject);
                assert_eq!(
                    thread.place,
                    OutboundPlace::Recorded(Some("1793000000.001400".to_string())),
                    "{subject:?}"
                );
                // The message's posts go into the thread, and record nothing.
                thread.posted(Some("1793000000.001401"), NOW);
                assert_eq!(thread.thread_ts(), Some("1793000000.001400"));
            }
        }
        assert_eq!(
            rows(&records),
            vec![node_row(WF_THREAD, "1793000000.001400", NOW - DAY_MS, NOW)]
        );
        let mut reply = inbound(Some("1793000000.001400"));
        map_inbound_thread(
            Some(&records),
            &mut reply,
            Some("1793000000.001400"),
            NOW + 1,
        );
        assert_eq!(reply.conversation.thread_id.as_deref(), Some(WF_THREAD));

        let mut new = resolve_marked(Some(&records), Some(WF_THREAD), None, Subject::New);
        assert_eq!(new.place, OutboundPlace::NewPost);
        new.posted(Some("1793000000.001500"), NOW + 2);
        assert_eq!(
            rows(&records),
            vec![node_row(WF_THREAD, "1793000000.001500", NOW + 2, NOW + 2)]
        );
        assert_eq!(
            resolve_marked(Some(&records), Some(WF_THREAD), None, Subject::Same).thread_ts(),
            Some("1793000000.001500")
        );
    }

    /// On an unrecorded thread id every mark goes as `new`: to this channel's reply target, or to a
    /// new post at the top level, recorded. A place a person opened takes every message, whatever
    /// its mark, and so does the node without records.
    #[test]
    fn a_mark_changes_nothing_on_an_unrecorded_thread_id_or_a_persons_place() {
        let records = records();
        let here = target(CHANNEL, Some("1793000000.001600"));
        for subject in [Subject::New, Subject::Same, Subject::End] {
            assert_eq!(
                resolve_marked(
                    Some(&records),
                    Some("thread:sha256:agent"),
                    Some(&here),
                    subject
                )
                .place,
                OutboundPlace::ReplyTarget(Some("1793000000.001600".to_string()))
            );
            assert_eq!(
                resolve_marked(Some(&records), Some(WF_THREAD), None, subject).place,
                OutboundPlace::NewPost
            );
            assert_eq!(
                resolve_marked(None, Some(WF_THREAD), None, subject).place,
                OutboundPlace::NewPost
            );
        }
        let mut same = resolve_marked(Some(&records), Some(WF_THREAD), None, Subject::Same);
        same.posted(Some("1793000000.001601"), NOW);
        assert_eq!(
            rows(&records),
            vec![node_row(WF_THREAD, "1793000000.001601", NOW, NOW)]
        );

        records
            .record(PERSON_THREAD, CHANNEL, "1793000000.001602", NOW)
            .expect("record");
        records
            .record_node_post(PERSON_THREAD, CHANNEL, "1793000000.001603", NOW)
            .expect("record");
        for subject in [Subject::New, Subject::Same, Subject::End] {
            assert_eq!(
                resolve_marked(Some(&records), Some(PERSON_THREAD), None, subject).place,
                OutboundPlace::Recorded(Some("1793000000.001602".to_string())),
                "{subject:?}"
            );
        }
    }

    /// The mark is the WF's block's `subject`: `new`, `same` or `end`. None, `null`, or one this
    /// node does not know is `new`, and so is a `subject` outside the block.
    #[test]
    fn the_mark_is_read_from_the_wf_block() {
        let context = |subject: Value| json!({"io": {}, "wf": {"trail": [], "subject": subject}});
        for (subject, mark) in [
            (json!("new"), Subject::New),
            (json!("same"), Subject::Same),
            (json!("end"), Subject::End),
            (json!("End"), Subject::New),
            (json!(null), Subject::New),
            (json!(1), Subject::New),
        ] {
            assert_eq!(
                Subject::of_context(Some(&context(subject.clone()))),
                mark,
                "{subject}"
            );
        }
        assert_eq!(
            Subject::of_context(Some(&json!({"wf": {"trail": []}}))),
            Subject::New
        );
        assert_eq!(
            Subject::of_context(Some(&json!({"subject": "end"}))),
            Subject::New
        );
        assert_eq!(Subject::of_context(None), Subject::New);
    }

    /// An open menu of the node's post `ts` on a thread id, at the channel's top level.
    fn menu(thread_id: &str, ts: &str) -> OpenMenu {
        OpenMenu {
            channel_id: CHANNEL.to_string(),
            ts: ts.to_string(),
            thread_ts: String::new(),
            thread_id: thread_id.to_string(),
            menu: json!({"options": [{"name": "handback", "description": "Hand it back."}]}),
            blocks: json!([{"type": "actions", "block_id": "wf_menu", "elements": []}]),
        }
    }

    /// The open menus (docs/wf-orchestration-v1.md §5.4.4): one per thread id in a channel. A new
    /// one takes the older's place and returns it, to close its post; `end` takes it; a post with
    /// no open menu has none. Other thread ids, and other channels, keep theirs.
    #[test]
    fn a_thread_id_has_one_open_menu_per_channel() {
        let records = records();
        let first = menu(WF_THREAD, "1793000000.001700");
        assert_eq!(records.open_menu(&first, NOW).expect("open"), None);
        assert_eq!(
            records
                .menu_of_post(CHANNEL, "1793000000.001700")
                .expect("lookup"),
            Some(first.clone())
        );

        let other_thread = menu(PERSON_THREAD, "1793000000.001701");
        let mut other_channel = menu(WF_THREAD, "1793000000.001702");
        other_channel.channel_id = "C0OTHER".to_string();
        assert_eq!(records.open_menu(&other_thread, NOW).expect("open"), None);
        assert_eq!(records.open_menu(&other_channel, NOW).expect("open"), None);

        let mut second = menu(WF_THREAD, "1793000000.001703");
        second.thread_ts = "1793000000.001700".to_string();
        assert_eq!(
            records.open_menu(&second, NOW + 1).expect("open"),
            Some(first)
        );
        assert_eq!(
            records
                .menu_of_post(CHANNEL, "1793000000.001700")
                .expect("lookup"),
            None
        );
        assert_eq!(
            records
                .menu_of_post(CHANNEL, "1793000000.001703")
                .expect("lookup"),
            Some(second.clone())
        );

        assert_eq!(
            records
                .take_menu_of_thread(WF_THREAD, CHANNEL)
                .expect("take"),
            Some(second)
        );
        assert_eq!(
            records
                .take_menu_of_thread(WF_THREAD, CHANNEL)
                .expect("take"),
            None
        );
        assert_eq!(
            records
                .menu_of_post(CHANNEL, "1793000000.001701")
                .expect("lookup"),
            Some(other_thread)
        );
        assert_eq!(
            records
                .menu_of_post("C0OTHER", "1793000000.001702")
                .expect("lookup"),
            Some(other_channel)
        );
    }

    /// A file from before the menus gains their table when it opens, in place, and keeps its
    /// records. A menu open for 30 days is forgotten at start and by the daily pass.
    #[test]
    fn an_older_file_gains_the_menus_and_old_menus_are_forgotten() {
        let dir = temp_node_dir("menus");
        {
            let conn = Connection::open(dir.join(THREAD_RECORDS_FILE)).expect("file");
            conn.execute_batch(
                "CREATE TABLE slack_threads (
                    thread_id      TEXT NOT NULL,
                    channel_id     TEXT NOT NULL,
                    thread_ts      TEXT NOT NULL,
                    created_at     INTEGER NOT NULL,
                    last_used_at   INTEGER NOT NULL,
                    opened_by_node INTEGER NOT NULL DEFAULT 0,
                    PRIMARY KEY (channel_id, thread_ts)
                );",
            )
            .expect("the schema before the menus");
            conn.execute(
                "INSERT INTO slack_threads VALUES (?1, ?2, ?3, ?4, ?4, 1)",
                params![WF_THREAD, CHANNEL, "1793000000.001800", NOW],
            )
            .expect("a record");
        }
        let records = open_thread_records(&dir, NOW).expect("open");
        assert_eq!(
            rows(&records),
            vec![node_row(WF_THREAD, "1793000000.001800", NOW, NOW)]
        );
        records
            .open_menu(&menu(WF_THREAD, "1793000000.001800"), NOW - 31 * DAY_MS)
            .expect("open");
        records
            .open_menu(&menu(PERSON_THREAD, "1793000000.001801"), NOW - 29 * DAY_MS)
            .expect("open");
        drop(records);

        // At start.
        let records = open_thread_records(&dir, NOW).expect("reopen");
        assert_eq!(
            records
                .menu_of_post(CHANNEL, "1793000000.001800")
                .expect("lookup"),
            None
        );
        assert!(records
            .menu_of_post(CHANNEL, "1793000000.001801")
            .expect("lookup")
            .is_some());
        // The daily pass, two days on.
        assert_eq!(records.expire_menus(NOW + 2 * DAY_MS).expect("expire"), 1);
        assert_eq!(
            records
                .menu_of_post(CHANNEL, "1793000000.001801")
                .expect("lookup"),
            None
        );
        drop(records);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// When the menus fail, or a row does not read, the calls say so and nothing panics: the caller
    /// logs it, and a click is answered as on a closed menu (slack_menu). A take that cannot read
    /// its row undoes its delete.
    #[test]
    fn when_the_menus_fail_the_calls_say_so() {
        let records = records();
        records
            .open_menu(&menu(WF_THREAD, "1793000000.001900"), NOW)
            .expect("open");
        records
            .conn()
            .execute("UPDATE slack_menus SET menu = 'not json'", [])
            .expect("a row that does not read");
        assert!(records.menu_of_post(CHANNEL, "1793000000.001900").is_err());
        assert!(records.take_menu_of_thread(WF_THREAD, CHANNEL).is_err());
        let left: i64 = records
            .conn()
            .query_row("SELECT COUNT(*) FROM slack_menus", [], |row| row.get(0))
            .expect("count");
        assert_eq!(left, 1);

        records
            .conn()
            .execute_batch("DROP TABLE slack_menus")
            .expect("drop");
        assert!(records
            .open_menu(&menu(WF_THREAD, "1793000000.001901"), NOW)
            .is_err());
        assert!(records.expire_menus(NOW).is_err());
    }
}
