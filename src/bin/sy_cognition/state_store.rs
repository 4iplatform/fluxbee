//! SY.cognition's own state (docs/cognition-hands-v1.md §11.3): one SQLite file in its folder on its
//! hive, one row per communication with its whole state, and one per episode it keeps, the public
//! news of §12. Only this node reads it: the folder is 0700, and the file and its WAL sidecars 0600
//! from their creation.

use std::fs::{self, OpenOptions};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use super::episodes::Episode;
use super::{CognitionError, CommunicationKey, ThreadCognitionState};

/// The file's name in SY.cognition's folder.
pub(crate) const STORE_FILE_NAME: &str = "cognition.db";
/// The shape of a row's state. A field added with a default keeps it; a change older rows cannot
/// take raises it. A row of an older shape, or one that cannot be read, starts its communication
/// again; a row of a newer shape (after a rollback) is kept as it is, and its turns are skipped.
/// 2 (0.1.104): the feelings are `feelings`, since an episode is now a public event
/// (docs/cognition-hands-v1.md §12).
const STATE_VERSION: i64 = 2;

const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS communications (
    thread_id     TEXT NOT NULL,
    agent_ilk     TEXT NOT NULL,
    last_seen_ms  INTEGER NOT NULL,
    state_version INTEGER NOT NULL,
    state         TEXT NOT NULL,
    PRIMARY KEY (thread_id, agent_ilk)
);
CREATE INDEX IF NOT EXISTS idx_communications_last_seen ON communications (last_seen_ms);
CREATE TABLE IF NOT EXISTS episodes (
    episode_id  TEXT PRIMARY KEY,
    fades_at_ms INTEGER NOT NULL,
    episode     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_episodes_fades_at ON episodes (fades_at_ms);
"#;

const UPSERT_SQL: &str = r#"
INSERT INTO communications (thread_id, agent_ilk, last_seen_ms, state_version, state)
VALUES (?1, ?2, ?3, ?4, ?5)
ON CONFLICT (thread_id, agent_ilk) DO UPDATE SET
    last_seen_ms = excluded.last_seen_ms,
    state_version = excluded.state_version,
    state = excluded.state
"#;

/// The file and its one connection. Its calls are short and never wait on the network, so they run
/// where they are made.
pub(crate) struct CognitionStore {
    path: PathBuf,
    conn: Mutex<Connection>,
}

impl CognitionStore {
    /// Opens the file in `dir`, making both what they must be first.
    pub(crate) fn open(dir: &Path) -> Result<Self, CognitionError> {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        let path = dir.join(STORE_FILE_NAME);
        // Made 0600 before SQLite opens it: SQLite gives its WAL and shared-memory files the mode of
        // the database file.
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let conn = Connection::open(&path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA_SQL)?;
        for suffix in ["-wal", "-shm"] {
            let mut sidecar = path.clone().into_os_string();
            sidecar.push(suffix);
            let sidecar = PathBuf::from(sidecar);
            if sidecar.exists() {
                fs::set_permissions(&sidecar, fs::Permissions::from_mode(0o600))?;
            }
        }
        Ok(Self {
            path,
            conn: Mutex::new(conn),
        })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// A communication's state, `None` when the file has none, or one it cannot read: the
    /// communication starts again. `Err` for SQLite, and for a row of a newer shape.
    pub(crate) fn load(
        &self,
        key: &CommunicationKey,
    ) -> Result<Option<ThreadCognitionState>, CognitionError> {
        let conn = self.connection();
        let row: Option<(i64, String)> = conn
            .query_row(
                "SELECT state_version, state FROM communications WHERE thread_id = ?1 AND agent_ilk = ?2",
                params![key.thread_id, key.agent_ilk],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        match row {
            None => Ok(None),
            Some((version, _)) if version > STATE_VERSION => Err(format!(
                "its row is of a newer state version ({version}); it is kept as it is"
            )
            .into()),
            Some((STATE_VERSION, state)) => match serde_json::from_str(&state) {
                Ok(state) => Ok(Some(state)),
                Err(err) => {
                    tracing::warn!(
                        thread_id = %key.thread_id,
                        agent_ilk = %key.agent_ilk,
                        error = %err,
                        "sy.cognition could not read a communication in its file; it starts again"
                    );
                    Ok(None)
                }
            },
            Some((version, _)) => {
                tracing::warn!(
                    thread_id = %key.thread_id,
                    agent_ilk = %key.agent_ilk,
                    version,
                    "sy.cognition found a communication of an older state version; it starts again"
                );
                Ok(None)
            }
        }
    }

    /// Writes each communication's state whole, all in one transaction. Returns how many.
    pub(crate) fn save<'a>(
        &self,
        rows: impl IntoIterator<Item = (&'a CommunicationKey, &'a ThreadCognitionState)>,
    ) -> Result<u64, CognitionError> {
        let mut conn = self.connection();
        let tx = conn.transaction()?;
        let mut written = 0u64;
        {
            let mut upsert = tx.prepare_cached(UPSERT_SQL)?;
            for (key, state) in rows {
                upsert.execute(params![
                    key.thread_id,
                    key.agent_ilk,
                    super::thread_recency_epoch_ms(state),
                    STATE_VERSION,
                    serde_json::to_string(state)?,
                ])?;
                written += 1;
            }
        }
        tx.commit()?;
        Ok(written)
    }

    /// The most recently active communications with an agent, newest first, until their states take
    /// `max_bytes`; those with no agent (`no_agent`), whose package reaches no one, are left out. A
    /// row that cannot be read is left out: it starts again at its next turn.
    pub(crate) fn most_recent(
        &self,
        max_bytes: usize,
        no_agent: &str,
    ) -> Result<Vec<(CommunicationKey, ThreadCognitionState)>, CognitionError> {
        let conn = self.connection();
        let mut select = conn.prepare(
            "SELECT thread_id, agent_ilk, state FROM communications WHERE state_version = ?1 \
             AND agent_ilk <> ?2 ORDER BY last_seen_ms DESC",
        )?;
        let mut rows = select.query(params![STATE_VERSION, no_agent])?;
        let mut out = Vec::new();
        let mut bytes = 0usize;
        while let Some(row) = rows.next()? {
            let state: String = row.get(2)?;
            if bytes >= max_bytes {
                break;
            }
            bytes = bytes.saturating_add(state.len());
            let key = CommunicationKey {
                thread_id: row.get(0)?,
                agent_ilk: row.get(1)?,
            };
            match serde_json::from_str(&state) {
                Ok(state) => out.push((key, state)),
                Err(err) => tracing::warn!(
                    thread_id = %key.thread_id,
                    agent_ilk = %key.agent_ilk,
                    error = %err,
                    "sy.cognition could not read a communication in its file; it starts again"
                ),
            }
        }
        Ok(out)
    }

    /// The thread's communication whose agent served it last: the newest last turn
    /// (`last_turn_epoch_ms`) and its agent node, the one with no agent (`no_agent`) left out. A
    /// row that cannot be read is skipped.
    pub(crate) fn last_agent_on_thread(
        &self,
        thread_id: &str,
        no_agent: &str,
    ) -> Result<Option<(i64, String)>, CognitionError> {
        let conn = self.connection();
        let mut select = conn.prepare(
            "SELECT state FROM communications WHERE thread_id = ?1 AND agent_ilk <> ?2 \
             AND state_version = ?3",
        )?;
        let mut rows = select.query(params![thread_id, no_agent, STATE_VERSION])?;
        let mut last: Option<(i64, String)> = None;
        while let Some(row) = rows.next()? {
            let state: String = row.get(0)?;
            let Ok(state) = serde_json::from_str::<ThreadCognitionState>(&state) else {
                continue;
            };
            if state.agent_node.trim().is_empty() {
                continue;
            }
            let last_turn_ms = super::last_turn_epoch_ms(&state);
            if last.as_ref().is_none_or(|(ms, _)| last_turn_ms > *ms) {
                last = Some((last_turn_ms, state.agent_node));
            }
        }
        Ok(last)
    }

    /// Keeps an episode, once: false when the file had it already.
    pub(crate) fn save_episode(&self, episode: &Episode) -> Result<bool, CognitionError> {
        let inserted = self.connection().execute(
            "INSERT OR IGNORE INTO episodes (episode_id, fades_at_ms, episode) VALUES (?1, ?2, ?3)",
            params![
                episode.episode_id,
                episode.fades_at_ms(),
                serde_json::to_string(episode)?
            ],
        )?;
        Ok(inserted > 0)
    }

    /// The episodes not faded by `now_ms`. One that cannot be read is left out, and leaves with
    /// the next sweep that reaches its time.
    pub(crate) fn live_episodes(&self, now_ms: i64) -> Result<Vec<Episode>, CognitionError> {
        let conn = self.connection();
        let mut select =
            conn.prepare("SELECT episode_id, episode FROM episodes WHERE fades_at_ms > ?1")?;
        let mut rows = select.query(params![now_ms])?;
        let mut out = Vec::new();
        while let Some(row) = rows.next()? {
            let episode_id: String = row.get(0)?;
            let episode: String = row.get(1)?;
            match serde_json::from_str(&episode) {
                Ok(episode) => out.push(episode),
                Err(err) => tracing::warn!(
                    episode_id = %episode_id,
                    error = %err,
                    "sy.cognition could not read an episode in its file; it is left out"
                ),
            }
        }
        Ok(out)
    }

    /// Deletes the episodes faded by `now_ms`; returns how many.
    pub(crate) fn sweep_episodes(&self, now_ms: i64) -> Result<u64, CognitionError> {
        let deleted = self.connection().execute(
            "DELETE FROM episodes WHERE fades_at_ms <= ?1",
            params![now_ms],
        )?;
        Ok(deleted as u64)
    }

    /// Deletes every communication and every episode, in one transaction, and returns how many of
    /// each (FINDINGS A-160). The file is then rebuilt without the pages they freed and its WAL
    /// emptied, so what was deleted stays neither in the file nor beside it. That second step
    /// failing is logged: the rows are gone all the same.
    pub(crate) fn clear(&self) -> Result<(u64, u64), CognitionError> {
        let mut conn = self.connection();
        let tx = conn.transaction()?;
        let communications = tx.execute("DELETE FROM communications", [])?;
        let episodes = tx.execute("DELETE FROM episodes", [])?;
        tx.commit()?;
        let compacted = conn.execute("VACUUM", []).and_then(|_| {
            conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                row.get::<_, i64>(0)
            })
        });
        match compacted {
            Ok(0) => {}
            Ok(_) => tracing::warn!(
                "sy.cognition cleared its file, and its WAL could not be emptied: a reader held it"
            ),
            Err(err) => tracing::warn!(
                error = %err,
                "sy.cognition cleared its file, and could not rebuild it without the freed pages"
            ),
        }
        Ok((communications as u64, episodes as u64))
    }

    /// How many communications the file keeps, and its bytes with its WAL.
    pub(crate) fn stats(&self) -> Result<(u64, u64), CognitionError> {
        let communications: i64 =
            self.connection()
                .query_row("SELECT count(*) FROM communications", [], |row| row.get(0))?;
        let mut wal = self.path.clone().into_os_string();
        wal.push("-wal");
        let bytes = [self.path.clone(), PathBuf::from(wal)]
            .iter()
            .filter_map(|path| fs::metadata(path).ok())
            .map(|metadata| metadata.len())
            .sum();
        Ok((communications.max(0) as u64, bytes))
    }

    fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        // A panic while it was held leaves the connection usable: SQLite rolls back the open
        // transaction.
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
