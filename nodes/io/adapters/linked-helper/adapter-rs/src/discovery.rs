use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{types::ValueRef, Connection, OptionalExtension, Row};
use serde_json::{json, Value};

use crate::cloud_client::AdapterDiscoveryRequestItem;

/**
 * Result of scanning a Linked Helper Partitions directory.
 */
pub struct ScanResult {
    pub instances: Vec<ScannedDiscoveryInstance>,
    pub warnings: Vec<String>,
}

/**
 * Minimal discovery summary extracted from one linked-helper-account-xxxxxx-main folder.
 */
pub struct ScannedDiscoveryInstance {
    pub local_instance_id: String,
    pub local_path: String,
    pub has_lh_db: bool,
    pub has_preferences_json: bool,
    pub li_account_id: Option<i64>,
    pub li_external_id: Option<String>,
    pub matches_local_instance_id: Option<bool>,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub avatar_url: Option<String>,
    pub last_login_at: Option<String>,
    pub account_created_at: Option<String>,
    pub account_updated_at: Option<String>,
    pub lh_user_id: Option<i64>,
    pub lh_user_external_id: Option<String>,
    pub lh_user_last_login_at: Option<String>,
    pub chats_count: Option<i64>,
    pub pending_messages_count: Option<i64>,
    pub campaigns_count: Option<i64>,
    pub active_campaigns_count: Option<i64>,
    pub paused_campaigns_count: Option<i64>,
    pub archived_campaigns_count: Option<i64>,
    pub has_active_campaigns: Option<bool>,
    pub preferences_mw_state: Option<String>,
}

/**
 * Builds one manual discovery item without touching the filesystem.
 */
pub fn build_manual_discovery_item(
    local_instance_id: String,
    local_path: Option<String>,
    account_display_name: Option<String>,
    account_email: Option<String>,
    account_fingerprint: Option<String>,
) -> AdapterDiscoveryRequestItem {
    AdapterDiscoveryRequestItem {
        local_instance_id,
        local_path,
        account_fingerprint,
        account_hint: if account_display_name.is_some() || account_email.is_some() {
            Some(json!({
                "displayName": account_display_name,
                "email": account_email,
            }))
        } else {
            None
        },
        metadata: Some(json!({
            "source": "manual-cli",
        })),
    }
}

/**
 * Scans one Linked Helper Partitions root, finds ...-main folders, and reads li_accounts from lh.db.
 */
pub fn scan_linkedhelper_partitions(
    partitions_root: &str,
) -> Result<ScanResult, Box<dyn std::error::Error>> {
    let root = Path::new(partitions_root);
    if !root.exists() {
        return Err(format!("Partitions root does not exist: {}", partitions_root).into());
    }

    let mut instances = Vec::new();
    let mut warnings = Vec::new();

    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let Some(folder_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };

        if !folder_name.starts_with("linked-helper-account-") || !folder_name.ends_with("-main") {
            continue;
        }

        let local_instance_id = match extract_local_instance_id(folder_name) {
            Some(value) => value,
            None => {
                warnings.push(format!(
                    "Could not extract local_instance_id from folder [{}]",
                    folder_name
                ));
                continue;
            }
        };

        let lh_db_path = path.join("lh.db");
        let preferences_path = path.join("preferences.json");
        let mut scanned = ScannedDiscoveryInstance {
            local_instance_id: local_instance_id.clone(),
            local_path: path_to_string(&path),
            has_lh_db: lh_db_path.exists(),
            has_preferences_json: preferences_path.exists(),
            li_account_id: None,
            li_external_id: None,
            matches_local_instance_id: None,
            display_name: None,
            email: None,
            avatar_url: None,
            last_login_at: None,
            account_created_at: None,
            account_updated_at: None,
            lh_user_id: None,
            lh_user_external_id: None,
            lh_user_last_login_at: None,
            chats_count: None,
            pending_messages_count: None,
            campaigns_count: None,
            active_campaigns_count: None,
            paused_campaigns_count: None,
            archived_campaigns_count: None,
            has_active_campaigns: None,
            preferences_mw_state: None,
        };

        if lh_db_path.exists() {
            match read_lh_db_summary(&lh_db_path) {
                Ok(summary) => {
                    scanned.li_account_id = summary.li_account.li_account_id;
                    scanned.li_external_id = summary.li_account.li_external_id.clone();
                    scanned.matches_local_instance_id =
                        summary.li_account.li_external_id.as_ref().map(|value| value == &local_instance_id);
                    scanned.display_name = summary.li_account.display_name;
                    scanned.email = summary.li_account.email;
                    scanned.avatar_url = summary.li_account.avatar_url;
                    scanned.last_login_at = summary.li_account.last_login_at;
                    scanned.account_created_at = summary.li_account.account_created_at;
                    scanned.account_updated_at = summary.li_account.account_updated_at;
                    scanned.lh_user_id = summary.lh_user.as_ref().and_then(|item| item.lh_user_id);
                    scanned.lh_user_external_id = summary
                        .lh_user
                        .as_ref()
                        .and_then(|item| item.lh_user_external_id.clone());
                    scanned.lh_user_last_login_at = summary
                        .lh_user
                        .as_ref()
                        .and_then(|item| item.last_login_at.clone());
                    scanned.chats_count = summary.activity.as_ref().and_then(|item| item.chats_count);
                    scanned.pending_messages_count = summary
                        .activity
                        .as_ref()
                        .and_then(|item| item.pending_messages_count);
                    scanned.campaigns_count = summary.activity.as_ref().and_then(|item| item.campaigns_count);
                    scanned.active_campaigns_count = summary
                        .activity
                        .as_ref()
                        .and_then(|item| item.active_campaigns_count);
                    scanned.paused_campaigns_count = summary
                        .activity
                        .as_ref()
                        .and_then(|item| item.paused_campaigns_count);
                    scanned.archived_campaigns_count = summary
                        .activity
                        .as_ref()
                        .and_then(|item| item.archived_campaigns_count);
                    scanned.has_active_campaigns = summary
                        .activity
                        .as_ref()
                        .and_then(|item| item.has_active_campaigns);

                    if summary.li_account.li_account_id.is_none() {
                        warnings.push(format!(
                            "Instance [{}] has lh.db but li_accounts returned no rows",
                            local_instance_id
                        ));
                    }
                }
                Err(error) => warnings.push(format!(
                    "Failed to read lh.db for instance [{}]: {}",
                    local_instance_id, error
                )),
            }
        } else {
            warnings.push(format!(
                "Instance [{}] has no lh.db at [{}]",
                local_instance_id,
                path_to_string(&lh_db_path)
            ));
        }

        if preferences_path.exists() {
            match read_preferences_mw_state(&preferences_path) {
                Ok(value) => {
                    scanned.preferences_mw_state = value;
                }
                Err(error) => warnings.push(format!(
                    "Failed to read preferences.json for instance [{}]: {}",
                    local_instance_id, error
                )),
            }
        }

        instances.push(scanned);
    }

    Ok(ScanResult { instances, warnings })
}

struct LiAccountSummary {
    li_account_id: Option<i64>,
    li_external_id: Option<String>,
    display_name: Option<String>,
    email: Option<String>,
    avatar_url: Option<String>,
    last_login_at: Option<String>,
    account_created_at: Option<String>,
    account_updated_at: Option<String>,
}

struct LhUserSummary {
    lh_user_id: Option<i64>,
    lh_user_external_id: Option<String>,
    last_login_at: Option<String>,
}

struct ActivitySummary {
    chats_count: Option<i64>,
    pending_messages_count: Option<i64>,
    campaigns_count: Option<i64>,
    active_campaigns_count: Option<i64>,
    paused_campaigns_count: Option<i64>,
    archived_campaigns_count: Option<i64>,
    has_active_campaigns: Option<bool>,
}

struct LhDbSummary {
    li_account: LiAccountSummary,
    lh_user: Option<LhUserSummary>,
    activity: Option<ActivitySummary>,
}

/**
 * Reads the minimum useful discovery summary from a real Linked Helper lh.db file.
 */
fn read_lh_db_summary(db_path: &Path) -> Result<LhDbSummary, Box<dyn std::error::Error>> {
    let connection = Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;

    let li_account = read_li_account_summary(&connection)?;
    let lh_user = read_lh_user_summary(&connection)?;
    let activity = read_activity_summary(&connection)?;

    Ok(LhDbSummary {
        li_account,
        lh_user,
        activity,
    })
}

/**
 * Reads the most relevant li_accounts row for discovery display and identity hints.
 */
fn read_li_account_summary(
    connection: &Connection,
) -> Result<LiAccountSummary, Box<dyn std::error::Error>> {
    let mut statement = connection.prepare(
        "SELECT id, external_id, full_name, email, avatar, last_login_at, created_at, updated_at
         FROM li_accounts
         ORDER BY updated_at DESC, created_at DESC, id DESC
         LIMIT 1",
    )?;

    let summary = statement
        .query_row([], |row| {
            Ok(LiAccountSummary {
                li_account_id: row.get::<_, Option<i64>>(0)?,
                li_external_id: read_optional_string_like_value(row, 1)?,
                display_name: row.get::<_, Option<String>>(2)?,
                email: row.get::<_, Option<String>>(3)?,
                avatar_url: row.get::<_, Option<String>>(4)?,
                last_login_at: row.get::<_, Option<String>>(5)?,
                account_created_at: row.get::<_, Option<String>>(6)?,
                account_updated_at: row.get::<_, Option<String>>(7)?,
            })
        })
        .optional()?;

    Ok(summary.unwrap_or(LiAccountSummary {
        li_account_id: None,
        li_external_id: None,
        display_name: None,
        email: None,
        avatar_url: None,
        last_login_at: None,
        account_created_at: None,
        account_updated_at: None,
    }))
}

/**
 * Reads a minimal lh_users summary when the table exists in the local database.
 */
fn read_lh_user_summary(
    connection: &Connection,
) -> Result<Option<LhUserSummary>, Box<dyn std::error::Error>> {
    if !table_exists(connection, "lh_users")? {
        return Ok(None);
    }

    let mut statement = connection.prepare(
        "SELECT id, external_id, last_login_at
         FROM lh_users
         ORDER BY updated_at DESC, created_at DESC, id DESC
         LIMIT 1",
    )?;

    let summary = statement
        .query_row([], |row| {
            Ok(LhUserSummary {
                lh_user_id: row.get::<_, Option<i64>>(0)?,
                lh_user_external_id: read_optional_string_like_value(row, 1)?,
                last_login_at: row.get::<_, Option<String>>(2)?,
            })
        })
        .optional()?;

    Ok(summary)
}

/**
 * Reads optional aggregate counts that help Cloud show instance activity without syncing business data.
 */
fn read_activity_summary(
    connection: &Connection,
) -> Result<Option<ActivitySummary>, Box<dyn std::error::Error>> {
    let chats_count = count_rows_if_table_exists(connection, "chats")?;
    let pending_messages_count = count_rows_if_table_exists(connection, "pending_messages")?;
    let campaigns_count = count_rows_if_table_exists(connection, "campaigns")?;

    let campaign_status = if table_exists(connection, "campaigns")? {
        Some(read_campaign_status_summary(connection)?)
    } else {
        None
    };

    if chats_count.is_none() && pending_messages_count.is_none() && campaigns_count.is_none() && campaign_status.is_none() {
        return Ok(None);
    }

    let (active_campaigns_count, paused_campaigns_count, archived_campaigns_count, has_active_campaigns) =
        campaign_status.unwrap_or((None, None, None, None));

    Ok(Some(ActivitySummary {
        chats_count,
        pending_messages_count,
        campaigns_count,
        active_campaigns_count,
        paused_campaigns_count,
        archived_campaigns_count,
        has_active_campaigns,
    }))
}

/**
 * Reads a lightweight campaigns summary using the tentative active definition documented for discovery v1.
 */
fn read_campaign_status_summary(
    connection: &Connection,
) -> Result<
    (
        Option<i64>,
        Option<i64>,
        Option<i64>,
        Option<bool>,
    ),
    Box<dyn std::error::Error>,
> {
    let mut statement = connection.prepare(
        "SELECT
            SUM(CASE WHEN is_archived = 0 AND is_paused = 0 AND is_valid = 1 THEN 1 ELSE 0 END) AS active_campaigns_count,
            SUM(CASE WHEN is_paused = 1 THEN 1 ELSE 0 END) AS paused_campaigns_count,
            SUM(CASE WHEN is_archived = 1 THEN 1 ELSE 0 END) AS archived_campaigns_count
         FROM campaigns",
    )?;

    let counts = statement.query_row([], |row| {
        let active = row.get::<_, Option<i64>>(0)?;
        let paused = row.get::<_, Option<i64>>(1)?;
        let archived = row.get::<_, Option<i64>>(2)?;

        Ok((active, paused, archived, active.map(|value| value > 0)))
    })?;

    Ok(counts)
}

/**
 * Reads a JSON campaign status hint from preferences.json without coupling the adapter to the full schema.
 */
fn read_preferences_mw_state(
    preferences_path: &Path,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(preferences_path)?;
    let parsed: Value = serde_json::from_str(&text)?;

    Ok(parsed
        .get("app")
        .and_then(|value| value.get("restoreStates"))
        .and_then(|value| value.get("-1"))
        .and_then(|value| value.get("mwState"))
        .and_then(|value| value.as_str())
        .map(|value| value.to_string()))
}

fn count_rows_if_table_exists(
    connection: &Connection,
    table_name: &str,
) -> Result<Option<i64>, Box<dyn std::error::Error>> {
    if !table_exists(connection, table_name)? {
        return Ok(None);
    }

    let sql = format!("SELECT COUNT(*) FROM {}", table_name);
    let count = connection.query_row(&sql, [], |row| row.get::<_, i64>(0))?;
    Ok(Some(count))
}

fn table_exists(
    connection: &Connection,
    table_name: &str,
) -> Result<bool, Box<dyn std::error::Error>> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1 LIMIT 1",
            [table_name],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);

    Ok(exists)
}

/**
 * Reads one SQLite column that may arrive as TEXT or INTEGER and normalizes it to String.
 */
fn read_optional_string_like_value(
    row: &Row<'_>,
    index: usize,
) -> Result<Option<String>, rusqlite::Error> {
    match row.get_ref(index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Text(value) => Ok(Some(String::from_utf8_lossy(value).to_string())),
        ValueRef::Integer(value) => Ok(Some(value.to_string())),
        ValueRef::Real(value) => Ok(Some(value.to_string())),
        ValueRef::Blob(_) => Err(rusqlite::Error::InvalidColumnType(
            index,
            row.as_ref()
                .column_name(index)
                .unwrap_or("")
                .to_string(),
            rusqlite::types::Type::Blob,
        )),
    }
}

fn extract_local_instance_id(folder_name: &str) -> Option<String> {
    let prefix = "linked-helper-account-";
    let suffix = "-main";
    let without_prefix = folder_name.strip_prefix(prefix)?;
    let without_suffix = without_prefix.strip_suffix(suffix)?;
    if without_suffix.is_empty() {
        return None;
    }

    Some(without_suffix.to_string())
}

fn path_to_string(path: &PathBuf) -> String {
    path.to_string_lossy().to_string()
}
