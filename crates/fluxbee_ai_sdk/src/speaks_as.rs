//! The person an agent speaks as (docs/digital-executive-v1-stage-2.md §6.1): the conscious of a
//! Digital Executive answers as its ED, so its solution replies carry the ED's ILK as `src_ilk`.
//!
//! - **Its config names the person** (`RuntimeConfig::speaks_as`, an `ilk:<uuid>`), written at
//!   launch and read at boot.
//! - **Identity confirms it:** the agent's own ILK's entry in its hive's identity SHM works for
//!   that person (`works_for`), in the role `conscious` (`work_role`). Neither alone suffices: a
//!   config written by hand that identity does not confirm, or a relation in identity that no
//!   config names, changes nothing.
//! - **Never from the message:** the person is never copied from the incoming message (its
//!   `dst_ilk`), or whoever writes one would choose who an agent speaks as (design §7.2).
//!
//! The runtime reads its own entry once before its first message and again every
//! `SPEAKS_AS_CHECK_INTERVAL` (`SpeaksAsCheck`). While the person is not confirmed it behaves as
//! without one, says why in its status (`SpeaksAsStatus`) and WARNs at most once a minute. With
//! no person in its config it reads nothing and nothing changes.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use fluxbee_sdk::identity::{find_ilk_by_id_from_hive_id, IdentityIlkOption};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::time::Instant;

/// The role an agent's ILK holds toward the person it speaks as (`work_role` in the identity
/// SHM, docs/digital-executive-v1-stage-2.md §1.1).
pub const SPEAKS_AS_WORK_ROLE: &str = "conscious";

/// How often the runtime reads its own ILK's entry again: ai.generic's definition poll's default
/// (`poll_interval_secs`, 10 s).
pub const SPEAKS_AS_CHECK_INTERVAL: Duration = Duration::from_secs(10);

/// The key of a `NODE_STATUS_GET` answer's `extensions` that holds the status.
pub const SPEAKS_AS_STATUS_KEY: &str = "speaks_as";

/// At most one WARN this often while the person is not confirmed.
const SPEAKS_AS_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// The person the config names, as the runtime uses it: trimmed, and None when absent or blank.
pub(crate) fn configured_person(speaks_as: Option<&str>) -> Option<&str> {
    speaks_as.map(str::trim).filter(|value| !value.is_empty())
}

/// Whether identity confirms `speaks_as`, the person the config names: the agent's own ILK,
/// `self_ilk`, read by `own_entry`, works for it in the role `conscious`. The person as identity
/// names it, or why not. `own_entry` reads one active ILK's entry by its id, `Ok(None)` when the
/// region does not hold it.
pub fn confirm_speaks_as(
    speaks_as: &str,
    self_ilk: Option<&str>,
    own_entry: impl FnOnce(&str) -> std::result::Result<Option<IdentityIlkOption>, String>,
) -> std::result::Result<String, String> {
    let speaks_as = speaks_as.trim();
    if !is_ilk_id(speaks_as) {
        return Err(format!("speaks_as {speaks_as:?} is not an ilk:<uuid>"));
    }
    let Some(self_ilk) = self_ilk.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err("this node has no ILK of its own (FLUXBEE_NODE_ILK_ID is unset)".to_string());
    };
    let Some(entry) = own_entry(self_ilk)? else {
        return Err(format!(
            "the identity SHM does not hold this node's ILK {self_ilk} (absent or not active)"
        ));
    };
    let Some(works_for) = entry.works_for.as_deref() else {
        return Err(format!(
            "identity names no person this node's ILK {self_ilk} works for"
        ));
    };
    if !works_for.eq_ignore_ascii_case(speaks_as) {
        return Err(format!(
            "identity says this node's ILK {self_ilk} works for {works_for}, not {speaks_as}"
        ));
    }
    match entry.work_role.as_deref() {
        Some(role) if role.eq_ignore_ascii_case(SPEAKS_AS_WORK_ROLE) => Ok(works_for.to_string()),
        role => Err(format!(
            "identity gives this node's ILK {self_ilk} the role {} toward {speaks_as}, not \
             {SPEAKS_AS_WORK_ROLE}",
            role.unwrap_or("none")
        )),
    }
}

/// An `ilk:<uuid>`.
fn is_ilk_id(value: &str) -> bool {
    value
        .strip_prefix("ilk:")
        .is_some_and(|uuid| uuid::Uuid::parse_str(uuid).is_ok())
}

/// One active ILK's entry in the identity SHM of the hive `node_name` runs on (`<name>@<hive>`).
pub(crate) fn own_entry_in_shm(
    node_name: &str,
    ilk_id: &str,
) -> std::result::Result<Option<IdentityIlkOption>, String> {
    let hive = node_name
        .rsplit_once('@')
        .map(|(_, hive)| hive.trim())
        .filter(|hive| !hive.is_empty())
        .ok_or_else(|| format!("the node's name {node_name:?} names no hive"))?;
    find_ilk_by_id_from_hive_id(hive, ilk_id)
        .map_err(|err| format!("the identity SHM of hive {hive} cannot be read: {err}"))
}

/// What a node's status says of the person its config says it speaks as
/// (`extensions.speaks_as` of its `NODE_STATUS_GET` answer). None while its config names none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeaksAsStatus {
    /// The person the config names (`speaks_as`).
    pub ilk: String,
    /// Whether identity confirms it: the node's solution replies carry it while true, and the
    /// node's own ILK while false.
    pub confirmed: bool,
    /// Why identity does not confirm it, while it does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl SpeaksAsStatus {
    /// Puts the status in `payload`, a `NODE_STATUS_GET` answer, as `extensions.speaks_as`, beside
    /// any other extension. Its health stays as it is: a person not confirmed yet is the state of
    /// a conscious just born, before its relation is set.
    pub fn add_to_status(&self, payload: &mut Value) {
        let Some(fields) = payload.as_object_mut() else {
            return;
        };
        let extensions = fields
            .entry("extensions".to_string())
            .or_insert_with(|| json!({}));
        if !extensions.is_object() {
            *extensions = json!({});
        }
        extensions[SPEAKS_AS_STATUS_KEY] = serde_json::to_value(self).unwrap_or_default();
    }
}

/// The runtime's view of the person it speaks as: the last check's outcome, which `stamp_reply`
/// reads at each reply (`confirmed`) and a node's status reads (`status`). Empty while the config
/// names no person.
#[derive(Debug, Default)]
pub(crate) struct SpeaksAsCheck {
    state: Mutex<SpeaksAsState>,
}

#[derive(Debug, Default)]
struct SpeaksAsState {
    status: Option<SpeaksAsStatus>,
    /// The person as identity names it, while confirmed: what a solution reply carries.
    confirmed: Option<String>,
    last_warned: Option<Instant>,
}

impl SpeaksAsCheck {
    /// The person a solution reply carries now: Some only while identity confirms it.
    pub(crate) fn confirmed(&self) -> Option<String> {
        self.lock().confirmed.clone()
    }

    pub(crate) fn status(&self) -> Option<SpeaksAsStatus> {
        self.lock().status.clone()
    }

    /// Takes one check's outcome (`confirm_speaks_as`) for `speaks_as`, at `now`. A change is a
    /// line of its own: INFO once confirmed, WARN once not; while not, a WARN at most once a
    /// minute names the node, the person and why. Returns whether it WARNed.
    pub(crate) fn record(
        &self,
        node_name: &str,
        speaks_as: &str,
        outcome: std::result::Result<String, String>,
        now: Instant,
    ) -> bool {
        let mut state = self.lock();
        let was_confirmed = state.confirmed.is_some();
        match outcome {
            Ok(person) => {
                if !was_confirmed {
                    tracing::info!(
                        node_name = %node_name,
                        speaks_as = %person,
                        "the agent speaks as the person its config names: identity confirms it \
                         (its ILK works for that person, as its conscious)"
                    );
                }
                state.status = Some(SpeaksAsStatus {
                    ilk: speaks_as.to_string(),
                    confirmed: true,
                    reason: None,
                });
                state.confirmed = Some(person);
                false
            }
            Err(reason) => {
                let warn = state
                    .last_warned
                    .is_none_or(|last| now.duration_since(last) >= SPEAKS_AS_WARN_INTERVAL);
                if warn {
                    tracing::warn!(
                        node_name = %node_name,
                        speaks_as = %speaks_as,
                        reason = %reason,
                        was_confirmed,
                        "the agent does not speak as the person its config names: identity does \
                         not confirm it, so its replies carry its own ILK"
                    );
                    state.last_warned = Some(now);
                }
                state.status = Some(SpeaksAsStatus {
                    ilk: speaks_as.to_string(),
                    confirmed: false,
                    reason: Some(reason),
                });
                state.confirmed = None;
                warn
            }
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SpeaksAsState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED: &str = "ilk:7a1f0c3e-1111-4c2b-9d3e-0123456789ab";
    const OTHER: &str = "ilk:7a1f0c3e-2222-4c2b-9d3e-0123456789ab";
    const AGENT: &str = "ilk:7a1f0c3e-3333-4c2b-9d3e-0123456789ab";

    fn entry(works_for: Option<&str>, work_role: Option<&str>) -> IdentityIlkOption {
        IdentityIlkOption {
            ilk_id: AGENT.to_string(),
            tenant_id: "tnt:00000000-0000-4000-8000-00000000000a".to_string(),
            display_name: None,
            handler_node: Some("AI.laura@worker1".to_string()),
            registration_status: "complete".to_string(),
            ilk_type: "agent".to_string(),
            role_hash: None,
            skill_hashes: Vec::new(),
            handbook_hashes: Vec::new(),
            personality_hash: None,
            is_virtual: false,
            works_for: works_for.map(str::to_string),
            work_role: work_role.map(str::to_string),
        }
    }

    fn check(
        speaks_as: &str,
        self_ilk: Option<&str>,
        read: std::result::Result<Option<IdentityIlkOption>, String>,
    ) -> std::result::Result<String, String> {
        confirm_speaks_as(speaks_as, self_ilk, |ilk| {
            assert_eq!(ilk, AGENT, "only the node's own ILK is read");
            read
        })
    }

    /// Confirmed only while the agent's own ILK works for the configured person, as its conscious.
    #[test]
    fn identity_confirms_the_person_only_through_the_agents_own_entry() {
        let confirmed = check(
            ED,
            Some(AGENT),
            Ok(Some(entry(Some(ED), Some("conscious")))),
        );
        assert_eq!(confirmed.as_deref(), Ok(ED));
        // The config's spelling does not matter; the person is identity's.
        let upper = ED.to_ascii_uppercase().replacen("ILK:", "ilk:", 1);
        assert_eq!(
            check(
                &upper,
                Some(AGENT),
                Ok(Some(entry(Some(ED), Some("conscious"))))
            )
            .as_deref(),
            Ok(ED)
        );

        let why = |outcome: std::result::Result<String, String>| outcome.unwrap_err();
        assert!(
            why(check(ED, Some(AGENT), Ok(Some(entry(None, None))))).contains("names no person")
        );
        assert!(why(check(
            ED,
            Some(AGENT),
            Ok(Some(entry(Some(OTHER), Some("conscious"))))
        ))
        .contains(&format!("works for {OTHER}, not {ED}")));
        assert!(why(check(
            ED,
            Some(AGENT),
            Ok(Some(entry(Some(ED), Some("channel"))))
        ))
        .contains("the role channel"));
        assert!(why(check(ED, Some(AGENT), Ok(Some(entry(Some(ED), None))))).contains("role none"));
        assert!(why(check(ED, Some(AGENT), Ok(None))).contains("does not hold this node's ILK"));
        assert!(why(check(ED, Some(AGENT), Err("no region".to_string()))).contains("no region"));
    }

    /// Without an ILK of its own, or with a person that is no ILK, nothing is read.
    #[test]
    fn nothing_is_read_without_an_own_ilk_or_a_person_that_is_an_ilk() {
        let unread = |speaks_as: &str, self_ilk: Option<&str>| {
            confirm_speaks_as(speaks_as, self_ilk, |_| panic!("nothing is read")).unwrap_err()
        };
        assert!(unread(ED, None).contains("no ILK of its own"));
        assert!(unread(ED, Some("  ")).contains("no ILK of its own"));
        assert!(unread("laura", Some(AGENT)).contains("is not an ilk:<uuid>"));
        assert!(unread("ilk:laura", Some(AGENT)).contains("is not an ilk:<uuid>"));
    }

    /// A reply carries the person only while the last check confirms it; the status says why not,
    /// and the WARN comes at most once a minute.
    #[test]
    fn the_check_keeps_the_last_outcome_and_warns_at_most_once_a_minute() {
        let check = SpeaksAsCheck::default();
        assert_eq!(check.confirmed(), None);
        assert_eq!(check.status(), None, "nothing until the first check");

        let start = Instant::now();
        assert!(check.record("AI.laura@worker1", ED, Err("not yet".to_string()), start));
        assert_eq!(check.confirmed(), None);
        assert_eq!(
            check.status(),
            Some(SpeaksAsStatus {
                ilk: ED.to_string(),
                confirmed: false,
                reason: Some("not yet".to_string())
            })
        );
        assert!(!check.record(
            "AI.laura@worker1",
            ED,
            Err("not yet".to_string()),
            start + Duration::from_secs(10)
        ));

        assert!(!check.record("AI.laura@worker1", ED, Ok(ED.to_string()), start));
        assert_eq!(check.confirmed().as_deref(), Some(ED));
        assert_eq!(
            check.status(),
            Some(SpeaksAsStatus {
                ilk: ED.to_string(),
                confirmed: true,
                reason: None
            })
        );

        // The relation cleared in identity: the next check takes the person away at once.
        assert!(!check.record(
            "AI.laura@worker1",
            ED,
            Err("cleared".to_string()),
            start + Duration::from_secs(30)
        ));
        assert_eq!(check.confirmed(), None);
        assert!(check.record(
            "AI.laura@worker1",
            ED,
            Err("cleared".to_string()),
            start + Duration::from_secs(60)
        ));
    }

    #[test]
    fn the_status_goes_in_the_extensions_beside_the_others() {
        let mut payload = json!({
            "status": "ok",
            "health_state": "HEALTHY",
            "extensions": {"ai_runtime": {"state": "serving"}}
        });
        SpeaksAsStatus {
            ilk: ED.to_string(),
            confirmed: false,
            reason: Some("not yet".to_string()),
        }
        .add_to_status(&mut payload);
        assert_eq!(payload["health_state"], json!("HEALTHY"));
        assert_eq!(
            payload["extensions"]["ai_runtime"]["state"],
            json!("serving")
        );
        assert_eq!(
            payload["extensions"]["speaks_as"],
            json!({"ilk": ED, "confirmed": false, "reason": "not yet"})
        );

        let mut bare = json!({"status": "ok"});
        SpeaksAsStatus {
            ilk: ED.to_string(),
            confirmed: true,
            reason: None,
        }
        .add_to_status(&mut bare);
        assert_eq!(
            bare["extensions"]["speaks_as"],
            json!({"ilk": ED, "confirmed": true})
        );
    }

    #[test]
    fn a_blank_person_is_no_person() {
        assert_eq!(configured_person(None), None);
        assert_eq!(configured_person(Some("  ")), None);
        assert_eq!(configured_person(Some(" ilk:x ")), Some("ilk:x"));
    }

    #[test]
    fn the_region_is_the_hive_of_the_nodes_name() {
        let err = own_entry_in_shm("AI.laura", AGENT).unwrap_err();
        assert!(err.contains("names no hive"), "{err}");
    }
}
