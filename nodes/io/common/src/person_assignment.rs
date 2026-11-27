#![forbid(unsafe_code)]

//! The person an IO carries (docs/digital-executive-v1-stage-2.md §7, design §7.1): a Digital
//! Executive's channel, for one. Its inbound messages name that person as `dst_ilk`.
//!
//! - **Its config assigns it:** `io.person_ilk`, an `ilk:<uuid>` next to `io.dst_node`, written at
//!   launch. The node hands it to its `InboundProcessor` (`InboundConfig::person_ilk`, and
//!   `InboundProcessor::set_person_ilk` on a config change), as it hands on `io.dst_node`.
//! - **Identity confirms it** (`confirm_person_assignment`): the IO's own ILK's entry in the
//!   identity SHM works for that person (`works_for`) in the role `channel` (`work_role`), and the
//!   person is of the IO's own tenant. Neither alone suffices: a config written by hand that
//!   identity does not confirm, or a relation in identity that no config names, stamps nothing.
//! - **Checked** when the person is set and then at most every `PERSON_RECHECK_INTERVAL`, as
//!   messages come in (`InboundProcessor::refresh_person_ilk` checks at once).
//! - **While confirmed** each inbound message carries the person as `dst_ilk`; **while not**, none,
//!   a WARN at most once a minute names the IO, the person and why, and the status says why
//!   (`PersonAssignmentStatus`). Either way the message goes where it went: `io.dst_node`.
//! - **With no person in the config** nothing is read and nothing changes.

use std::time::{Duration, Instant};

use fluxbee_sdk::identity::IdentityIlkOption;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::identity::IdentityResolver;

/// The key of the config's `io` block that assigns the person: `io.person_ilk`.
pub const PERSON_ILK_CONFIG_KEY: &str = "person_ilk";

/// The role an IO's ILK holds toward the person it carries (`work_role` in the identity SHM,
/// docs/digital-executive-v1-stage-2.md §1.1).
pub const PERSON_CHANNEL_WORK_ROLE: &str = "channel";

/// How long a check of the person holds before the next message checks again.
pub const PERSON_RECHECK_INTERVAL: Duration = Duration::from_secs(10);

/// The key of a status answer's `extensions` that holds the status.
pub const PERSON_STATUS_KEY: &str = "person_ilk";

/// At most one WARN this often while the person is not confirmed.
const PERSON_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// `io.person_ilk` of an effective config: None when absent, blank or not a string.
pub fn extract_runtime_person_ilk(effective_config: Option<&Value>) -> Option<String> {
    effective_config
        .and_then(|config| config.get("io"))
        .and_then(|io| io.get(PERSON_ILK_CONFIG_KEY))
        .and_then(Value::as_str)
        .and_then(|value| normalize_person_ilk(Some(value)))
}

/// A person as the processor keeps it: trimmed, and None when blank.
pub(crate) fn normalize_person_ilk(person_ilk: Option<&str>) -> Option<String> {
    person_ilk
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Whether identity confirms `person_ilk`, the person the config assigns: the IO's own ILK,
/// `self_ilk`, works for it in the role `channel`, and the person is of the IO's own tenant,
/// `self_tenant`. Both entries are read from the identity SHM through `identity`
/// (`IdentityResolver::ilk_entry`). The person as identity names it, or why not.
pub fn confirm_person_assignment(
    person_ilk: &str,
    self_ilk: Option<&str>,
    self_tenant: Option<&str>,
    identity: &dyn IdentityResolver,
) -> Result<String, String> {
    let person_ilk = person_ilk.trim();
    if !is_ilk_id(person_ilk) {
        return Err(format!("io.person_ilk {person_ilk:?} is not an ilk:<uuid>"));
    }
    let Some(self_ilk) = present(self_ilk) else {
        return Err("this IO has no ILK of its own (FLUXBEE_NODE_ILK_ID is unset)".to_string());
    };
    let Some(self_tenant) = present(self_tenant) else {
        return Err(
            "this IO has no tenant of its own (FLUXBEE_NODE_TENANT_ID is unset)".to_string(),
        );
    };
    let entry = |ilk: &str| -> Result<Option<IdentityIlkOption>, String> {
        identity
            .ilk_entry(ilk)
            .map_err(|err| format!("the identity SHM cannot be read: {err}"))
    };

    let Some(own) = entry(self_ilk)? else {
        return Err(format!(
            "the identity SHM does not hold this IO's ILK {self_ilk} (absent or not active)"
        ));
    };
    let Some(works_for) = own.works_for.as_deref() else {
        return Err(format!(
            "identity names no person this IO's ILK {self_ilk} works for"
        ));
    };
    if !works_for.eq_ignore_ascii_case(person_ilk) {
        return Err(format!(
            "identity says this IO's ILK {self_ilk} works for {works_for}, not {person_ilk}"
        ));
    }
    match own.work_role.as_deref() {
        Some(role) if role.eq_ignore_ascii_case(PERSON_CHANNEL_WORK_ROLE) => {}
        role => {
            return Err(format!(
                "identity gives this IO's ILK {self_ilk} the role {} toward {person_ilk}, not \
                 {PERSON_CHANNEL_WORK_ROLE}",
                role.unwrap_or("none")
            ))
        }
    }

    let Some(person) = entry(works_for)? else {
        return Err(format!(
            "the identity SHM does not hold the person {person_ilk} (absent or not active)"
        ));
    };
    if !person.tenant_id.trim().eq_ignore_ascii_case(self_tenant) {
        return Err(format!(
            "the person {person_ilk} is of tenant {}, not of this IO's tenant {self_tenant}",
            person.tenant_id
        ));
    }
    Ok(works_for.to_string())
}

fn present(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// An `ilk:<uuid>`.
fn is_ilk_id(value: &str) -> bool {
    value
        .strip_prefix("ilk:")
        .is_some_and(|uuid| uuid::Uuid::parse_str(uuid).is_ok())
}

/// What an IO's status says of the person its config assigns it (`extensions.person_ilk`). None
/// while its config assigns none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonAssignmentStatus {
    /// The person the config assigns (`io.person_ilk`).
    pub ilk: String,
    /// Whether identity confirms it: inbound messages carry it as `dst_ilk` while true.
    pub confirmed: bool,
    /// Why identity does not confirm it, while it does not; also before its first check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PersonAssignmentStatus {
    /// Puts the status in `payload`, a status answer (the managed `NODE_STATUS_GET` answer, or the
    /// `CONFIG_GET` and `STATUS` answers an IO builds), as `extensions.person_ilk`, beside any
    /// other extension. Its health stays as it is.
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
        extensions[PERSON_STATUS_KEY] = serde_json::to_value(self).unwrap_or_default();
    }
}

/// Who the IO is, for the check and its WARN.
pub(crate) struct IoSelf<'a> {
    pub node_uuid: &'a str,
    pub ilk: Option<&'a str>,
    pub tenant: Option<&'a str>,
}

/// The processor's view of the person its config assigns: the last check's outcome.
#[derive(Debug, Default)]
pub(crate) struct PersonAssignment {
    person_ilk: Option<String>,
    status: Option<PersonAssignmentStatus>,
    /// The person as identity names it, while confirmed: what an inbound message carries.
    confirmed: Option<String>,
    checked_at: Option<Instant>,
    last_warned: Option<Instant>,
}

impl PersonAssignment {
    pub(crate) fn new(person_ilk: Option<&str>) -> Self {
        let mut assignment = Self::default();
        assignment.set(person_ilk);
        assignment
    }

    /// Assigns `person_ilk` (None or blank for none). Another person than the one held is checked
    /// afresh: until then, nothing is stamped and the status says it is not checked yet.
    pub(crate) fn set(&mut self, person_ilk: Option<&str>) {
        let person_ilk = normalize_person_ilk(person_ilk);
        if person_ilk == self.person_ilk {
            return;
        }
        let last_warned = self.last_warned;
        *self = Self {
            status: person_ilk.as_ref().map(|ilk| PersonAssignmentStatus {
                ilk: ilk.clone(),
                confirmed: false,
                reason: Some("not checked against identity yet".to_string()),
            }),
            person_ilk,
            last_warned,
            ..Self::default()
        };
    }

    pub(crate) fn status(&self) -> Option<PersonAssignmentStatus> {
        self.status.clone()
    }

    /// The person an inbound message carries at `now`: checked again when the last check is older
    /// than `PERSON_RECHECK_INTERVAL`. None with no person, and while identity does not confirm it.
    pub(crate) fn person_at(
        &mut self,
        io: &IoSelf<'_>,
        identity: &dyn IdentityResolver,
        now: Instant,
    ) -> Option<String> {
        self.person_ilk.as_ref()?;
        let due = self
            .checked_at
            .is_none_or(|at| now.saturating_duration_since(at) >= PERSON_RECHECK_INTERVAL);
        if due {
            self.check(io, identity, now);
        }
        self.confirmed.clone()
    }

    /// Checks the person against identity at `now`. A change is a line of its own: INFO once
    /// confirmed; while not, a WARN at most once a minute names the IO, the person and why.
    /// Returns whether it WARNed.
    pub(crate) fn check(
        &mut self,
        io: &IoSelf<'_>,
        identity: &dyn IdentityResolver,
        now: Instant,
    ) -> bool {
        let Some(person_ilk) = self.person_ilk.clone() else {
            return false;
        };
        let outcome = confirm_person_assignment(&person_ilk, io.ilk, io.tenant, identity);
        self.checked_at = Some(now);
        let was_confirmed = self.confirmed.is_some();
        match outcome {
            Ok(person) => {
                if !was_confirmed {
                    tracing::info!(
                        io_node_uuid = %io.node_uuid,
                        io_ilk = ?io.ilk,
                        person_ilk = %person,
                        "the IO carries the person its config assigns: identity confirms it (its \
                         ILK works for that person, as its channel); inbound messages name the \
                         person as dst_ilk"
                    );
                }
                self.status = Some(PersonAssignmentStatus {
                    ilk: person_ilk,
                    confirmed: true,
                    reason: None,
                });
                self.confirmed = Some(person);
                false
            }
            Err(reason) => {
                let warn = self
                    .last_warned
                    .is_none_or(|at| now.saturating_duration_since(at) >= PERSON_WARN_INTERVAL);
                if warn {
                    tracing::warn!(
                        io_node_uuid = %io.node_uuid,
                        io_ilk = ?io.ilk,
                        person_ilk = %person_ilk,
                        reason = %reason,
                        was_confirmed,
                        "the IO does not carry the person its config assigns: identity does not \
                         confirm it, so inbound messages name no dst_ilk (they still go to \
                         io.dst_node)"
                    );
                    self.last_warned = Some(now);
                }
                self.status = Some(PersonAssignmentStatus {
                    ilk: person_ilk,
                    confirmed: false,
                    reason: Some(reason),
                });
                self.confirmed = None;
                warn
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::identity::{IdentityError, ResolveOrCreateInput};
    use std::any::Any;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    pub(crate) const ED: &str = "ilk:9c4e2a10-0000-4b1c-8d2e-0000000000ed";
    pub(crate) const OTHER: &str = "ilk:9c4e2a10-0000-4b1c-8d2e-0000000000ff";
    pub(crate) const IO_ILK: &str = "ilk:9c4e2a10-0000-4b1c-8d2e-000000000010";
    pub(crate) const TENANT: &str = "tnt:9c4e2a10-0000-4b1c-8d2e-0000000000aa";
    pub(crate) const OTHER_TENANT: &str = "tnt:9c4e2a10-0000-4b1c-8d2e-0000000000bb";

    pub(crate) fn ilk(
        ilk_id: &str,
        tenant_id: &str,
        works_for: Option<&str>,
        work_role: Option<&str>,
    ) -> IdentityIlkOption {
        IdentityIlkOption {
            ilk_id: ilk_id.to_string(),
            tenant_id: tenant_id.to_string(),
            display_name: None,
            handler_node: None,
            registration_status: "complete".to_string(),
            ilk_type: if works_for.is_some() {
                "agent".to_string()
            } else {
                "human".to_string()
            },
            role_hash: None,
            skill_hashes: Vec::new(),
            handbook_hashes: Vec::new(),
            personality_hash: None,
            is_virtual: false,
            works_for: works_for.map(str::to_string),
            work_role: work_role.map(str::to_string),
        }
    }

    /// An identity SHM of the test's own: the entries it holds, or a region that cannot be read.
    /// Every lookup of a writer hits `ilk:writer`; it counts the entries read.
    #[derive(Default)]
    pub(crate) struct Region {
        pub entries: Mutex<HashMap<String, IdentityIlkOption>>,
        pub unreadable: Mutex<bool>,
        pub reads: AtomicUsize,
    }

    impl Region {
        /// The IO's ILK working for the ED as its channel, and the ED in the IO's tenant.
        pub(crate) fn confirming() -> Self {
            let region = Self::default();
            region.put(ilk(IO_ILK, TENANT, Some(ED), Some("channel")));
            region.put(ilk(ED, TENANT, None, None));
            region
        }

        pub(crate) fn put(&self, entry: IdentityIlkOption) {
            self.entries
                .lock()
                .unwrap()
                .insert(entry.ilk_id.clone(), entry);
        }

        pub(crate) fn reads(&self) -> usize {
            self.reads.load(Ordering::SeqCst)
        }
    }

    impl IdentityResolver for Region {
        fn as_any(&self) -> &dyn Any {
            self
        }

        fn lookup(&self, _input: &ResolveOrCreateInput) -> Result<Option<String>, IdentityError> {
            Ok(Some("ilk:writer".to_string()))
        }

        fn ilk_entry(&self, ilk_id: &str) -> Result<Option<IdentityIlkOption>, IdentityError> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            if *self.unreadable.lock().unwrap() {
                return Err(IdentityError::Unavailable);
            }
            Ok(self.entries.lock().unwrap().get(ilk_id).cloned())
        }
    }

    fn confirm(region: &Region) -> Result<String, String> {
        confirm_person_assignment(ED, Some(IO_ILK), Some(TENANT), region)
    }

    #[test]
    fn identity_confirms_the_person_through_the_ios_own_entry_and_the_persons_tenant() {
        assert_eq!(confirm(&Region::confirming()).as_deref(), Ok(ED));

        let region = Region::confirming();
        region.put(ilk(IO_ILK, TENANT, None, None));
        assert!(confirm(&region).unwrap_err().contains("names no person"));

        region.put(ilk(IO_ILK, TENANT, Some(OTHER), Some("channel")));
        assert!(confirm(&region)
            .unwrap_err()
            .contains(&format!("works for {OTHER}, not {ED}")));

        region.put(ilk(IO_ILK, TENANT, Some(ED), Some("conscious")));
        assert!(confirm(&region).unwrap_err().contains("the role conscious"));

        region.put(ilk(IO_ILK, TENANT, Some(ED), None));
        assert!(confirm(&region).unwrap_err().contains("the role none"));

        // A person of another tenant is never this IO's.
        let region = Region::confirming();
        region.put(ilk(ED, OTHER_TENANT, None, None));
        assert!(confirm(&region)
            .unwrap_err()
            .contains(&format!("of tenant {OTHER_TENANT}")));

        let region = Region::confirming();
        region.entries.lock().unwrap().remove(ED);
        assert!(confirm(&region)
            .unwrap_err()
            .contains("does not hold the person"));

        let region = Region::confirming();
        region.entries.lock().unwrap().remove(IO_ILK);
        assert!(confirm(&region)
            .unwrap_err()
            .contains("does not hold this IO's ILK"));

        let region = Region::confirming();
        *region.unreadable.lock().unwrap() = true;
        assert!(confirm(&region).unwrap_err().contains("cannot be read"));
    }

    #[test]
    fn nothing_is_read_without_an_own_ilk_a_tenant_or_a_person_that_is_an_ilk() {
        let region = Region::confirming();
        let unread = |person: &str, ilk: Option<&str>, tenant: Option<&str>| {
            confirm_person_assignment(person, ilk, tenant, &region).unwrap_err()
        };
        assert!(unread(ED, None, Some(TENANT)).contains("no ILK of its own"));
        assert!(unread(ED, Some(IO_ILK), Some(" ")).contains("no tenant of its own"));
        assert!(unread("laura", Some(IO_ILK), Some(TENANT)).contains("not an ilk:<uuid>"));
        assert_eq!(region.reads(), 0);
    }

    #[test]
    fn the_key_sits_next_to_dst_node() {
        let config =
            json!({"io": {"dst_node": "AI.laura@worker1", "person_ilk": format!(" {ED} ")}});
        assert_eq!(
            extract_runtime_person_ilk(Some(&config)).as_deref(),
            Some(ED)
        );
        for config in [
            json!({"io": {"dst_node": "AI.laura@worker1"}}),
            json!({"io": {"person_ilk": "  "}}),
            json!({"io": {"person_ilk": 7}}),
            json!({}),
        ] {
            assert_eq!(extract_runtime_person_ilk(Some(&config)), None, "{config}");
        }
        assert_eq!(extract_runtime_person_ilk(None), None);
    }

    const IO: IoSelf<'static> = IoSelf {
        node_uuid: "uuid-io",
        ilk: Some(IO_ILK),
        tenant: Some(TENANT),
    };

    /// The check holds for `PERSON_RECHECK_INTERVAL`, then the next message checks again; the
    /// WARN comes at most once a minute; a new person is checked afresh.
    #[test]
    fn the_check_holds_for_its_interval_and_warns_at_most_once_a_minute() {
        let region = Region::confirming();
        let mut assignment = PersonAssignment::new(Some(ED));
        assert_eq!(
            assignment.status().map(|status| status.confirmed),
            Some(false),
            "not checked yet"
        );
        let start = Instant::now();
        assert_eq!(
            assignment.person_at(&IO, &region, start).as_deref(),
            Some(ED)
        );
        assert_eq!(region.reads(), 2);

        // Identity clears the relation: the person goes at the next check, not before.
        region.put(ilk(IO_ILK, TENANT, None, None));
        assert_eq!(
            assignment
                .person_at(&IO, &region, start + Duration::from_secs(9))
                .as_deref(),
            Some(ED)
        );
        assert_eq!(region.reads(), 2, "the check held");
        assert_eq!(
            assignment.person_at(&IO, &region, start + PERSON_RECHECK_INTERVAL),
            None
        );
        let status = assignment.status().expect("a status");
        assert!(!status.confirmed);
        assert!(status.reason.unwrap().contains("names no person"));

        let mut fresh = PersonAssignment::new(Some(ED));
        assert!(fresh.check(&IO, &region, start));
        assert!(!fresh.check(&IO, &region, start + Duration::from_secs(30)));
        assert!(fresh.check(&IO, &region, start + Duration::from_secs(60)));

        // Another person is checked afresh; the same one keeps its check.
        fresh.set(Some(ED));
        assert!(fresh.checked_at.is_some());
        fresh.set(Some(OTHER));
        assert!(fresh.checked_at.is_none());
        assert_eq!(fresh.status().unwrap().ilk, OTHER);
        fresh.set(Some("  "));
        assert_eq!(fresh.status(), None);
        let reads = region.reads();
        assert_eq!(fresh.person_at(&IO, &region, start), None);
        assert_eq!(region.reads(), reads, "no person, nothing read");
    }

    #[test]
    fn the_status_goes_in_the_extensions_beside_the_others() {
        let mut payload = json!({"ok": true, "extensions": {"web": {"open": 1}}});
        PersonAssignmentStatus {
            ilk: ED.to_string(),
            confirmed: false,
            reason: Some("not yet".to_string()),
        }
        .add_to_status(&mut payload);
        assert_eq!(payload["extensions"]["web"], json!({"open": 1}));
        assert_eq!(
            payload["extensions"]["person_ilk"],
            json!({"ilk": ED, "confirmed": false, "reason": "not yet"})
        );
    }
}
