use std::collections::BTreeMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::state::{AdapterDesiredBindingState, AdapterState};

/**
 * Snapshot loaded back from the runtime SQLite store to hydrate the adapter runtime state.
 */
pub struct RuntimeDbSnapshot {
    pub runtime_meta: BTreeMap<String, Option<String>>,
    pub desired_bindings: Vec<AdapterDesiredBindingState>,
    pub instance_runtime_states: Vec<InstanceRuntimeStateRecord>,
    pub sync_checkpoints: Vec<SyncCheckpointRecord>,
}

/**
 * Persisted per-instance runtime row stored in the local SQLite runtime store.
 */
#[derive(Debug, Clone)]
pub struct InstanceRuntimeStateRecord {
    pub local_instance_id: String,
    pub managed_instance_id: Option<String>,
    pub effective_status: String,
    pub last_event_at: Option<String>,
    pub last_sent_at: Option<String>,
    pub last_acked_at: Option<String>,
    pub last_checkpoint_ts: Option<String>,
    pub last_checkpoint_cursor: Option<String>,
    pub last_runtime_error_code: Option<String>,
    pub last_runtime_error_message: Option<String>,
}

/**
 * Persisted checkpoint row used to resume incremental sync per instance/channel.
 */
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct SyncCheckpointRecord {
    pub local_instance_id: String,
    pub channel: String,
    pub checkpoint_type: String,
    pub checkpoint_value: Option<String>,
    pub last_confirmed_sent_at: Option<String>,
}

/**
 * Opens the local runtime SQLite database and ensures the minimal schema exists.
 */
pub fn sync_runtime_db(
    db_path: &Path,
    state: &AdapterState,
) -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;
    write_runtime_meta(&connection, state)?;
    write_desired_bindings(&connection, state)?;
    write_instance_runtime_state(&connection, state)?;
    write_sync_checkpoints(&connection, state)?;
    Ok(())
}

/**
 * Loads the currently persisted runtime snapshot from SQLite when the DB already exists.
 */
pub fn load_runtime_snapshot(
    db_path: &Path,
) -> Result<Option<RuntimeDbSnapshot>, Box<dyn std::error::Error>> {
    if !db_path.exists() {
        return Ok(None);
    }

    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let mut runtime_meta = BTreeMap::new();
    {
        let mut statement = connection.prepare("SELECT key, value FROM runtime_meta")?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let key: String = row.get(0)?;
            let value: Option<String> = row.get(1)?;
            runtime_meta.insert(key, value);
        }
    }

    let mut desired_bindings = Vec::new();
    {
        let mut statement = connection.prepare(
            "
            SELECT local_instance_id, managed_instance_id, status, report_to_kind, report_to_url
            FROM desired_bindings
            ORDER BY local_instance_id ASC
            ",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            desired_bindings.push(AdapterDesiredBindingState {
                local_instance_id: row.get(0)?,
                managed_instance_id: row.get(1)?,
                status: row.get(2)?,
                report_to_kind: row.get(3)?,
                report_to_url: row.get(4)?,
                // The SQLite mirror does not track node-reported control state;
                // the JSON state is the source of truth for these.
                operational_state: None,
                last_node_directive: None,
            });
        }
    }

    let mut instance_runtime_states = Vec::new();
    {
        let mut statement = connection.prepare(
            "
            SELECT
              local_instance_id,
              managed_instance_id,
              effective_status,
              last_event_at,
              last_sent_at,
              last_acked_at,
              last_checkpoint_ts,
              last_checkpoint_cursor,
              last_runtime_error_code,
              last_runtime_error_message
            FROM instance_runtime_state
            ORDER BY local_instance_id ASC
            ",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            instance_runtime_states.push(InstanceRuntimeStateRecord {
                local_instance_id: row.get(0)?,
                managed_instance_id: row.get(1)?,
                effective_status: row.get(2)?,
                last_event_at: row.get(3)?,
                last_sent_at: row.get(4)?,
                last_acked_at: row.get(5)?,
                last_checkpoint_ts: row.get(6)?,
                last_checkpoint_cursor: row.get(7)?,
                last_runtime_error_code: row.get(8)?,
                last_runtime_error_message: row.get(9)?,
            });
        }
    }

    let mut sync_checkpoints = Vec::new();
    {
        let mut statement = connection.prepare(
            "
            SELECT
              local_instance_id,
              channel,
              checkpoint_type,
              checkpoint_value,
              last_confirmed_sent_at
            FROM sync_checkpoints
            ORDER BY local_instance_id ASC, channel ASC
            ",
        )?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            sync_checkpoints.push(SyncCheckpointRecord {
                local_instance_id: row.get(0)?,
                channel: row.get(1)?,
                checkpoint_type: row.get(2)?,
                checkpoint_value: row.get(3)?,
                last_confirmed_sent_at: row.get(4)?,
            });
        }
    }

    Ok(Some(RuntimeDbSnapshot {
        runtime_meta,
        desired_bindings,
        instance_runtime_states,
        sync_checkpoints,
    }))
}

/**
 * Reads one previously stored checkpoint for the given instance/channel pair.
 */
#[allow(dead_code)]
pub fn get_sync_checkpoint(
    db_path: &Path,
    local_instance_id: &str,
    channel: &str,
) -> Result<Option<SyncCheckpointRecord>, Box<dyn std::error::Error>> {
    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;

    let record = connection
        .query_row(
            "
            SELECT
              local_instance_id,
              channel,
              checkpoint_type,
              checkpoint_value,
              last_confirmed_sent_at
            FROM sync_checkpoints
            WHERE local_instance_id = ?1 AND channel = ?2
            ",
            params![local_instance_id, channel],
            |row| {
                Ok(SyncCheckpointRecord {
                    local_instance_id: row.get(0)?,
                    channel: row.get(1)?,
                    checkpoint_type: row.get(2)?,
                    checkpoint_value: row.get(3)?,
                    last_confirmed_sent_at: row.get(4)?,
                })
            },
        )
        .optional()?;

    Ok(record)
}

/**
 * Upserts one checkpoint row for the given instance/channel pair.
 */
#[allow(dead_code)]
pub fn upsert_sync_checkpoint(
    db_path: &Path,
    record: &SyncCheckpointRecord,
) -> Result<(), Box<dyn std::error::Error>> {
    let connection = Connection::open(db_path)?;
    initialize_schema(&connection)?;
    let now = super::current_timestamp_iso();

    connection.execute(
        "
        INSERT INTO sync_checkpoints (
          local_instance_id,
          channel,
          checkpoint_type,
          checkpoint_value,
          last_confirmed_sent_at,
          created_at,
          updated_at
        )
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(local_instance_id, channel) DO UPDATE SET
          checkpoint_type = excluded.checkpoint_type,
          checkpoint_value = excluded.checkpoint_value,
          last_confirmed_sent_at = excluded.last_confirmed_sent_at,
          updated_at = excluded.updated_at
        ",
        params![
            record.local_instance_id,
            record.channel,
            record.checkpoint_type,
            record.checkpoint_value,
            record.last_confirmed_sent_at,
            now,
            now
        ],
    )?;

    Ok(())
}

fn initialize_schema(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute_batch(
        "
        CREATE TABLE IF NOT EXISTS runtime_meta (
          key TEXT PRIMARY KEY,
          value TEXT
        );

        CREATE TABLE IF NOT EXISTS desired_bindings (
          local_instance_id TEXT PRIMARY KEY,
          managed_instance_id TEXT NOT NULL,
          status TEXT NOT NULL,
          report_to_kind TEXT,
          report_to_url TEXT,
          desired_state_version INTEGER,
          updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS instance_runtime_state (
          local_instance_id TEXT PRIMARY KEY,
          managed_instance_id TEXT,
          effective_status TEXT NOT NULL,
          last_event_at TEXT,
          last_sent_at TEXT,
          last_acked_at TEXT,
          last_checkpoint_ts TEXT,
          last_checkpoint_cursor TEXT,
          last_runtime_error_code TEXT,
          last_runtime_error_message TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS sync_checkpoints (
          local_instance_id TEXT NOT NULL,
          channel TEXT NOT NULL,
          checkpoint_type TEXT NOT NULL,
          checkpoint_value TEXT,
          last_confirmed_sent_at TEXT,
          created_at TEXT NOT NULL,
          updated_at TEXT NOT NULL,
          PRIMARY KEY (local_instance_id, channel)
        );
        ",
    )?;

    // Additive migration for pre-existing DBs created before report_to columns.
    ensure_column(connection, "desired_bindings", "report_to_kind", "TEXT")?;
    ensure_column(connection, "desired_bindings", "report_to_url", "TEXT")?;

    Ok(())
}

/**
 * Adds a column to a table when missing; treats "duplicate column" as success so
 * the migration is idempotent across runs and fresh vs. legacy databases.
 */
fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    column_type: &str,
) -> Result<(), rusqlite::Error> {
    let statement = format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}");
    match connection.execute(&statement, []) {
        Ok(_) => Ok(()),
        Err(rusqlite::Error::SqliteFailure(_, Some(message)))
            if message.contains("duplicate column name") =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn write_runtime_meta(connection: &Connection, state: &AdapterState) -> Result<(), rusqlite::Error> {
    let now = super::current_timestamp_iso();
    let desired_state_version = state
        .runtime
        .last_known_desired_state_version
        .map(|value| value.to_string());
    let instances_count = state
        .runtime
        .last_seen_instances_count
        .map(|value| value.to_string());

    let items: [(&str, Option<&str>); 13] = [
        ("adapter_status", state.runtime.adapter_status.as_deref()),
        (
            "last_successful_alive_at",
            state.runtime.last_successful_alive_at.as_deref(),
        ),
        (
            "last_successful_discovery_at",
            state.runtime.last_successful_discovery_at.as_deref(),
        ),
        ("last_scan_at", state.runtime.last_scan_at.as_deref()),
        ("last_error_code", state.runtime.last_error_code.as_deref()),
        ("last_error_message", state.runtime.last_error_message.as_deref()),
        (
            "last_discovery_hash",
            state.runtime.last_discovery_hash.as_deref(),
        ),
        ("lh_root_status", state.runtime.lh_root_status.as_deref()),
        (
            "cloud_last_response_status",
            state.runtime.cloud_last_response_status.as_deref(),
        ),
        ("adapter_id", Some(state.adapter_id.as_str())),
        ("tenant_id", Some(state.tenant_id.as_str())),
        ("lh_root_path", state.lh_root_path.as_deref()),
        ("adapter_build", Some(state.adapter_build.as_str())),
    ];

    for (key, value) in items {
        connection.execute(
            "
            INSERT INTO runtime_meta (key, value)
            VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            ",
            params![key, value],
        )?;
    }

    connection.execute(
        "
        INSERT INTO runtime_meta (key, value)
        VALUES ('last_known_desired_state_version', ?1)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        ",
        params![desired_state_version.as_deref()],
    )?;

    connection.execute(
        "
        INSERT INTO runtime_meta (key, value)
        VALUES ('last_seen_instances_count', ?1)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        ",
        params![instances_count.as_deref()],
    )?;

    connection.execute(
        "
        INSERT INTO runtime_meta (key, value)
        VALUES ('runtime_meta_synced_at', ?1)
        ON CONFLICT(key) DO UPDATE SET value = excluded.value
        ",
        params![now],
    )?;

    Ok(())
}

fn write_desired_bindings(connection: &Connection, state: &AdapterState) -> Result<(), rusqlite::Error> {
    let transaction = connection.unchecked_transaction()?;
    transaction.execute("DELETE FROM desired_bindings", [])?;

    let updated_at = super::current_timestamp_iso();
    let desired_state_version = state.runtime.last_known_desired_state_version;

    for binding in &state.runtime.desired_bindings {
        transaction.execute(
            "
            INSERT INTO desired_bindings (
              local_instance_id,
              managed_instance_id,
              status,
              report_to_kind,
              report_to_url,
              desired_state_version,
              updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            ",
            params![
                binding.local_instance_id,
                binding.managed_instance_id,
                binding.status,
                binding.report_to_kind,
                binding.report_to_url,
                desired_state_version,
                updated_at
            ],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

fn write_instance_runtime_state(connection: &Connection, state: &AdapterState) -> Result<(), rusqlite::Error> {
    let transaction = connection.unchecked_transaction()?;
    let now = super::current_timestamp_iso();
    let mut instances: BTreeMap<String, Option<String>> = BTreeMap::new();

    for instance in &state.runtime.cloud_discovered_instances {
        instances
            .entry(instance.local_instance_id.clone())
            .or_insert(instance.managed_instance_id.clone());
    }

    for binding in &state.runtime.desired_bindings {
        instances.insert(
            binding.local_instance_id.clone(),
            Some(binding.managed_instance_id.clone()),
        );
    }

    for (local_instance_id, managed_instance_id) in instances {
        let effective_status = derive_effective_instance_status(state, local_instance_id.as_str());
        let last_event_at = state.runtime.last_scan_at.clone();
        let cloud_discovery_checkpoint =
            load_checkpoint_from_connection(&transaction, local_instance_id.as_str(), "cloud_discovery")?;
        let last_sent_at = cloud_discovery_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.last_confirmed_sent_at.clone())
            .or_else(|| derive_last_sent_at(state, local_instance_id.as_str()));
        let last_checkpoint_ts = cloud_discovery_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.last_confirmed_sent_at.clone())
            .or_else(|| state.runtime.last_successful_discovery_at.clone());
        let last_checkpoint_cursor = cloud_discovery_checkpoint
            .as_ref()
            .and_then(|checkpoint| checkpoint.checkpoint_value.clone());
        transaction.execute(
            "
            INSERT INTO instance_runtime_state (
              local_instance_id,
              managed_instance_id,
              effective_status,
              last_event_at,
              last_sent_at,
              last_acked_at,
              last_checkpoint_ts,
              last_checkpoint_cursor,
              last_runtime_error_code,
              last_runtime_error_message,
              created_at,
              updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8, ?9, ?10, ?11)
            ON CONFLICT(local_instance_id) DO UPDATE SET
              managed_instance_id = excluded.managed_instance_id,
              effective_status = excluded.effective_status,
              last_event_at = excluded.last_event_at,
              last_sent_at = excluded.last_sent_at,
              last_checkpoint_ts = excluded.last_checkpoint_ts,
              last_checkpoint_cursor = excluded.last_checkpoint_cursor,
              last_runtime_error_code = excluded.last_runtime_error_code,
              last_runtime_error_message = excluded.last_runtime_error_message,
              updated_at = excluded.updated_at
            ",
            params![
                local_instance_id,
                managed_instance_id,
                effective_status,
                last_event_at,
                last_sent_at,
                last_checkpoint_ts,
                last_checkpoint_cursor,
                state.runtime.last_error_code.as_deref(),
                state.runtime.last_error_message.as_deref(),
                now,
                now
            ],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

fn load_checkpoint_from_connection(
    connection: &Connection,
    local_instance_id: &str,
    channel: &str,
) -> Result<Option<SyncCheckpointRecord>, rusqlite::Error> {
    connection
        .query_row(
            "
            SELECT
              local_instance_id,
              channel,
              checkpoint_type,
              checkpoint_value,
              last_confirmed_sent_at
            FROM sync_checkpoints
            WHERE local_instance_id = ?1 AND channel = ?2
            ",
            params![local_instance_id, channel],
            |row| {
                Ok(SyncCheckpointRecord {
                    local_instance_id: row.get(0)?,
                    channel: row.get(1)?,
                    checkpoint_type: row.get(2)?,
                    checkpoint_value: row.get(3)?,
                    last_confirmed_sent_at: row.get(4)?,
                })
            },
        )
        .optional()
}

fn write_sync_checkpoints(connection: &Connection, state: &AdapterState) -> Result<(), rusqlite::Error> {
    let transaction = connection.unchecked_transaction()?;
    let now = super::current_timestamp_iso();

    for instance in &state.runtime.cloud_discovered_instances {
        transaction.execute(
            "
            INSERT INTO sync_checkpoints (
              local_instance_id,
              channel,
              checkpoint_type,
              checkpoint_value,
              last_confirmed_sent_at,
              created_at,
              updated_at
            )
            VALUES (?1, 'cloud_discovery', 'hash', ?2, ?3, ?4, ?5)
            ON CONFLICT(local_instance_id, channel) DO UPDATE SET
              checkpoint_type = excluded.checkpoint_type,
              checkpoint_value = excluded.checkpoint_value,
              last_confirmed_sent_at = excluded.last_confirmed_sent_at,
              updated_at = excluded.updated_at
            ",
            params![
                instance.local_instance_id,
                state.runtime.last_discovery_hash.as_deref(),
                state.runtime.last_successful_discovery_at.as_deref(),
                now,
                now
            ],
        )?;
    }

    transaction.commit()?;
    Ok(())
}

fn derive_last_sent_at(state: &AdapterState, local_instance_id: &str) -> Option<String> {
    let was_discovered = state
        .runtime
        .cloud_discovered_instances
        .iter()
        .any(|instance| instance.local_instance_id == local_instance_id);

    if was_discovered {
        return state.runtime.last_successful_discovery_at.clone();
    }

    None
}

fn derive_effective_instance_status(state: &AdapterState, local_instance_id: &str) -> &'static str {
    if state.runtime.adapter_status.as_deref() == Some("needs_reenrollment") {
        return "paused";
    }

    if state.runtime.last_error_code.is_some() {
        return "degraded";
    }

    let has_desired_binding = state
        .runtime
        .desired_bindings
        .iter()
        .any(|binding| binding.local_instance_id == local_instance_id);
    let has_cloud_discovery = state
        .runtime
        .cloud_discovered_instances
        .iter()
        .any(|instance| instance.local_instance_id == local_instance_id);

    if has_desired_binding && has_cloud_discovery {
        return "ready";
    }

    if has_desired_binding {
        return "desired_only";
    }

    if has_cloud_discovery {
        return "discovered_only";
    }

    "unknown"
}
