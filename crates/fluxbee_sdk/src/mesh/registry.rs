//! The motherbee's hive registry (A-48): one directory per hive it joined, `<root>/<hive_id>/`,
//! holding the `info.yaml` SY.orchestrator writes (`hive_id`, `role`, `status`, and `join` while a
//! join runs). The orchestrator creates the directory when it accepts a join and deletes it after
//! the hive's REMOVE_HIVE_CLEANUP. Only the motherbee keeps one, and the motherbee is never in it.
//!
//! Two readings, apart on purpose:
//! - **`lists`, the loose one:** the directory exists. A join in flight counts, since the join
//!   needs the hive's WAN link: the router admits WAN peers by it.
//! - **`role`, the strict one:** the role of a hive that is connected and whose join is done.
//!   SY.admin gives a mesh caller its hive's role by it, and io.web admits only an ingress by it.
//!   It fails closed.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where SY.orchestrator keeps the registry, on the motherbee.
pub const HIVE_REGISTRY_DIR: &str = "/var/lib/fluxbee/hives";

/// A hive id as a join accepts one: 1 to 64 of `[A-Za-z0-9_-]`. It names a directory of the
/// registry, never a path (`..`, `/etc`), so an id that comes from a peer is checked with it
/// before it is joined to one.
pub fn is_valid_hive_id(hive_id: &str) -> bool {
    !hive_id.is_empty()
        && hive_id.len() <= 64
        && hive_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// A joined hive's role in the strict reading (`HiveRegistry::role`). The registry holds spokes
/// only, so there is no motherbee here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HiveRole {
    Worker,
    Ingress,
    Egress,
}

/// The registry under one directory: `HIVE_REGISTRY_DIR` on the motherbee (`motherbee`), any other
/// in tests. Every reading goes to disk, so it always sees the orchestrator's last write.
#[derive(Clone, Debug)]
pub struct HiveRegistry {
    root: PathBuf,
}

impl HiveRegistry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The motherbee's registry, at `HIVE_REGISTRY_DIR`.
    pub fn motherbee() -> Self {
        Self::new(HIVE_REGISTRY_DIR)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The loose reading: `hive_id` is a valid hive id whose directory exists, from the moment the
    /// orchestrator accepts its join, before any `info.yaml`.
    pub fn lists(&self, hive_id: &str) -> bool {
        // The hive id may come from a peer: one that is not a plain name (`..`, `/etc`) would look
        // outside the registry.
        is_valid_hive_id(hive_id) && self.root.join(hive_id).is_dir()
    }

    /// The strict reading: the role of `hive_id` once it is connected and its join is done
    /// (`HiveEntry::role`); `None` otherwise.
    pub fn role(&self, hive_id: &str) -> Option<HiveRole> {
        if !is_valid_hive_id(hive_id) {
            return None;
        }
        HiveEntry::read(&self.root.join(hive_id), hive_id.to_string()).role()
    }

    /// Every directory of the registry, in name order, each with its `info.yaml` as read now. The
    /// error is the registry's own directory missing or unreadable.
    pub fn entries(&self) -> std::io::Result<Vec<HiveEntry>> {
        let mut entries: Vec<HiveEntry> = std::fs::read_dir(&self.root)?
            .flatten()
            .filter(|entry| entry.path().is_dir())
            .map(|entry| {
                HiveEntry::read(
                    &entry.path(),
                    entry.file_name().to_string_lossy().into_owned(),
                )
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    /// Completes once `holds(self, hive_id)` is false, checked every `period` (not zero), the first
    /// time one period from now. What the registry admitted ends with it: the router cuts a WAN
    /// link once its hive is no longer listed (`lists`), and io.web a connection once its hive is
    /// no longer a connected ingress (`role`).
    pub async fn until_not<F>(&self, hive_id: &str, period: Duration, holds: F)
    where
        F: Fn(&HiveRegistry, &str) -> bool,
    {
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        loop {
            ticker.tick().await;
            if !holds(self, hive_id) {
                return;
            }
        }
    }
}

/// One directory of the registry and its `info.yaml`, as read.
#[derive(Clone, Debug)]
pub struct HiveEntry {
    name: String,
    /// `None` when `info.yaml` is missing (a join being accepted) or does not parse.
    info: Option<serde_json::Value>,
}

impl HiveEntry {
    fn read(dir: &Path, name: String) -> Self {
        let info = std::fs::read_to_string(dir.join("info.yaml"))
            .ok()
            .and_then(|raw| serde_yaml::from_str::<serde_json::Value>(&raw).ok());
        Self { name, info }
    }

    /// The directory's name: the hive id the orchestrator filed the entry under.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// A top-level text field of `info.yaml` (`hive_id`, `role`, `status`), as written.
    pub fn field(&self, key: &str) -> Option<&str> {
        self.info.as_ref()?.get(key)?.as_str()
    }

    /// The strict reading. The entry has a role only when its name is a valid hive id and its
    /// `info.yaml` names the same hive, says `status: connected`, has no join phase but `done`, and
    /// gives the role worker, ingress or egress, in any case. Anything else is `None`: no
    /// `info.yaml`, an entry for another hive, a join in flight, a failed hive, no role, or the
    /// role `motherbee`.
    pub fn role(&self) -> Option<HiveRole> {
        if !is_valid_hive_id(&self.name) {
            return None;
        }
        let info = self.info.as_ref()?;
        let text = |key: &str| {
            info.get(key)
                .and_then(|value| value.as_str())
                .map(str::trim)
        };
        // Only an explicit `done` ends a join: the flows rewrite the entry while they run, so a
        // join about to fail can read `connected` until its tail records the outcome.
        let join_done = info
            .get("join")
            .and_then(|join| join.get("phase"))
            .is_none_or(|phase| phase.as_str() == Some("done"));
        if text("hive_id") != Some(self.name.as_str())
            || text("status") != Some("connected")
            || !join_done
        {
            return None;
        }
        match text("role")?.to_ascii_lowercase().as_str() {
            "worker" => Some(HiveRole::Worker),
            "ingress" => Some(HiveRole::Ingress),
            "egress" => Some(HiveRole::Egress),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::test_registry;

    /// The loose reading: a directory is enough, a join being accepted included, and an id that is
    /// not a plain name never reaches a directory outside the registry.
    #[test]
    fn the_registry_lists_every_directory_with_a_valid_hive_id() {
        let registry = test_registry(&[("worker1", "hive_id: worker1\nstatus: joining\n")]);
        std::fs::create_dir(registry.root().join("accepting1")).unwrap();
        assert!(registry.lists("worker1"));
        assert!(registry.lists("accepting1"), "no info.yaml yet");
        assert!(!registry.lists("worker2"));
        for hive_id in ["", "..", ".", "/tmp", "worker1/..", "../worker1"] {
            assert!(!registry.lists(hive_id), "{hive_id:?}");
        }
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// The strict reading, and how it fails closed.
    #[test]
    fn a_hive_has_a_role_only_once_connected_with_its_join_done() {
        let done = "join:\n  phase: done\n";
        let entries = [
            (
                "worker1",
                format!("hive_id: worker1\nstatus: connected\nrole: worker\n{done}"),
            ),
            (
                "ingress1",
                format!("hive_id: ingress1\nstatus: connected\nrole: ingress\n{done}"),
            ),
            (
                "egress1",
                format!("hive_id: egress1\nstatus: connected\nrole: EGRESS\n{done}"),
            ),
            (
                "old1",
                "hive_id: old1\nstatus: connected\nrole: worker\n".to_string(),
            ),
            (
                "joining1",
                "hive_id: joining1\nstatus: joining\nrole: worker\njoin:\n  phase: accepted\n"
                    .to_string(),
            ),
            (
                "failed1",
                "hive_id: failed1\nstatus: failed\nrole: worker\n".to_string(),
            ),
            (
                "tail1",
                "hive_id: tail1\nstatus: connected\nrole: ingress\njoin:\n  phase: harden_ssh\n"
                    .to_string(),
            ),
            (
                "norole1",
                format!("hive_id: norole1\nstatus: connected\n{done}"),
            ),
            (
                "mother1",
                format!("hive_id: mother1\nstatus: connected\nrole: motherbee\n{done}"),
            ),
            (
                "other1",
                format!("hive_id: worker9\nstatus: connected\nrole: worker\n{done}"),
            ),
            ("garbled1", ": not yaml : [".to_string()),
        ];
        let entries: Vec<(&str, &str)> = entries
            .iter()
            .map(|(dir, info)| (*dir, info.as_str()))
            .collect();
        let registry = test_registry(&entries);
        assert_eq!(registry.role("worker1"), Some(HiveRole::Worker));
        assert_eq!(registry.role("old1"), Some(HiveRole::Worker));
        assert_eq!(registry.role("ingress1"), Some(HiveRole::Ingress));
        assert_eq!(registry.role("egress1"), Some(HiveRole::Egress));
        for hive in [
            "joining1",
            "failed1",
            "tail1",
            "norole1",
            "mother1",
            "other1",
            "garbled1",
            "gone1",
            "motherbee",
            "../worker1",
            "",
            "worker1/..",
        ] {
            assert_eq!(registry.role(hive), None, "{hive}");
        }
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// The entries are the registry's directories in name order, with their fields as written;
    /// a directory without `info.yaml` is one, a file is not.
    #[test]
    fn the_entries_are_the_registry_directories_in_name_order() {
        let registry = test_registry(&[
            (
                "worker1",
                "hive_id: worker1\nstatus: connected\nrole: worker\n",
            ),
            (
                "ingress1",
                "hive_id: ingress1\nstatus: joining\nrole: ingress\n",
            ),
        ]);
        std::fs::create_dir(registry.root().join("accepting1")).unwrap();
        std::fs::write(registry.root().join("stray-file"), "not a hive").unwrap();
        let entries = registry.entries().unwrap();
        let names: Vec<&str> = entries.iter().map(HiveEntry::name).collect();
        assert_eq!(names, ["accepting1", "ingress1", "worker1"]);
        assert_eq!(entries[0].field("status"), None);
        assert_eq!(entries[1].field("status"), Some("joining"));
        assert_eq!(entries[1].field("role"), Some("ingress"));
        assert_eq!(entries[1].role(), None);
        assert_eq!(entries[2].field("hive_id"), Some("worker1"));
        assert_eq!(entries[2].role(), Some(HiveRole::Worker));
        assert!(HiveRegistry::new(registry.root().join("absent"))
            .entries()
            .is_err());
        let _ = std::fs::remove_dir_all(registry.root());
    }

    /// The watcher fires once the hive's entry disappears (the router's `lists`) or once its role
    /// changes (io.web's connected ingress), and not while it holds.
    #[tokio::test]
    async fn the_watcher_fires_once_the_hive_no_longer_holds() {
        use tokio::time::timeout;
        let period = Duration::from_millis(20);
        let registry = test_registry(&[
            (
                "worker1",
                "hive_id: worker1\nstatus: connected\nrole: worker\n",
            ),
            (
                "ingress1",
                "hive_id: ingress1\nstatus: connected\nrole: ingress\n",
            ),
        ]);

        let removed = registry.until_not("worker1", period, HiveRegistry::lists);
        tokio::pin!(removed);
        assert!(
            timeout(Duration::from_millis(100), &mut removed)
                .await
                .is_err(),
            "worker1 is still listed"
        );
        std::fs::remove_dir_all(registry.root().join("worker1")).unwrap();
        timeout(Duration::from_secs(2), &mut removed)
            .await
            .expect("fires once the entry is gone");

        let demoted = registry.until_not("ingress1", period, |registry, hive| {
            registry.role(hive) == Some(HiveRole::Ingress)
        });
        tokio::pin!(demoted);
        assert!(
            timeout(Duration::from_millis(100), &mut demoted)
                .await
                .is_err(),
            "ingress1 is still a connected ingress"
        );
        std::fs::write(
            registry.root().join("ingress1").join("info.yaml"),
            "hive_id: ingress1\nstatus: connected\nrole: worker\n",
        )
        .unwrap();
        timeout(Duration::from_secs(2), &mut demoted)
            .await
            .expect("fires once the role changes");
        let _ = std::fs::remove_dir_all(registry.root());
    }
}
