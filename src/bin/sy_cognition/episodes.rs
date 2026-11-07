//! The episodes (docs/cognition-hands-v1.md §12): social events between people, public like the
//! news. The cognition whose turn showed one makes it once and sends it to every hive's cognition,
//! and each keeps a copy. An agent gets, in its package, those about the people of its
//! conversation. An episode fades with time, not with turns, and belongs to the people it is about:
//! the agent takes it into account to answer, and never passes it on.

use std::collections::{BTreeSet, HashMap};

use fluxbee_sdk::protocol::{MemoryTurnParty, NewsSummary};
use serde::{Deserialize, Serialize};

/// The lenses an episode is seen through, the lab's (§12.2): friction (conflict, hostility, what can
/// harm the bond), discomfort (tension, uncertainty, silence), appreciation (recognition, genuine
/// thanks) and shame (being judged, exposed or rejected).
pub(crate) const EPISODE_LENSES: [&str; 4] = ["friction", "discomfort", "appreciation", "shame"];
/// How an episode stood when it was told.
pub(crate) const EPISODE_STATES: [&str; 4] = ["resolved", "pending", "follow_up", "inconclusive"];
/// The gate (§12.4, the lab's): the final intensity, out of 10, from which an episode is made...
pub(crate) const EPISODE_MIN_INTENSITY: f64 = 7.0;
/// ...with evidence at least this strong, out of 10.
pub(crate) const EPISODE_MIN_EVIDENCE: f64 = 8.0;
/// An episode's intensity halves every 7 days...
pub(crate) const EPISODE_HALF_LIFE_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// ...and it leaves under this weight.
pub(crate) const EPISODE_FADED_BELOW: f64 = 2.0;
/// How many episodes a package carries, the strongest first.
pub(crate) const PACKAGE_MAX_NEWS: usize = 5;
/// The system message that carries a new episode to every hive's cognition, and its target.
pub(crate) const MSG_COGNITION_EPISODE: &str = "COGNITION_EPISODE";
pub(crate) const EPISODE_BROADCAST_TARGET: &str = "SY.cognition@*";
/// The longest title and summary an episode keeps, in characters.
const EPISODE_TITLE_MAX_CHARS: usize = 120;
const EPISODE_SUMMARY_MAX_CHARS: usize = 800;
/// The most people one episode is about.
const EPISODE_MAX_PEOPLE: usize = 4;

/// An episode as every cognition keeps it, in memory and in its file, and as it travels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Episode {
    pub(crate) episode_id: String,
    pub(crate) lens: String,
    pub(crate) title: String,
    /// A paragraph: what happened, between whom, and how it stood. Never what someone feels.
    pub(crate) summary: String,
    pub(crate) state: String,
    /// Out of 10, once the habit and the novelty adjusted the narrator's.
    pub(crate) intensity: f64,
    pub(crate) born_at_ms: i64,
    /// The ILKs of the people it is about.
    pub(crate) people: Vec<String>,
    /// Where it was made, kept though nobody needs it to read the episode.
    pub(crate) origin: EpisodeOrigin,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct EpisodeOrigin {
    pub(crate) thread_id: String,
    pub(crate) node: String,
}

impl Episode {
    /// Its weight at `now_ms`: its intensity, halved every 7 days.
    pub(crate) fn weight_at(&self, now_ms: i64) -> f64 {
        let age_ms = now_ms.saturating_sub(self.born_at_ms).max(0) as f64;
        self.intensity * 0.5f64.powf(age_ms / EPISODE_HALF_LIFE_MS as f64)
    }

    /// When its weight falls under `EPISODE_FADED_BELOW`, and it leaves.
    pub(crate) fn fades_at_ms(&self) -> i64 {
        let halvings = (self.intensity / EPISODE_FADED_BELOW).log2().max(0.0);
        self.born_at_ms
            .saturating_add((halvings * EPISODE_HALF_LIFE_MS as f64) as i64)
    }

    /// One that came from another cognition, as this one keeps it: a known lens and state, an
    /// intensity the gate would make, a title, a summary and a person, each within bounds. None for
    /// anything else.
    pub(crate) fn checked(mut self) -> Option<Self> {
        let known = |set: &[&str], value: &str| set.contains(&value);
        if self.episode_id.trim().is_empty()
            || !known(&EPISODE_LENSES, &self.lens)
            || !known(&EPISODE_STATES, &self.state)
            || !(EPISODE_MIN_INTENSITY..=10.0).contains(&self.intensity)
            || self.born_at_ms <= 0
        {
            return None;
        }
        self.title = cut(&self.title, EPISODE_TITLE_MAX_CHARS);
        self.summary = cut(&self.summary, EPISODE_SUMMARY_MAX_CHARS);
        self.people = people_of(self.people.iter().map(String::as_str));
        if self.title.is_empty() || self.summary.is_empty() || self.people.is_empty() {
            return None;
        }
        Some(self)
    }
}

/// What the narrator proposes: the model's half of an episode. The gate decides (`gate_episode`).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub(crate) struct EpisodeProposal {
    pub(crate) lens: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) state: String,
    /// Out of 10, before the habit and the novelty.
    pub(crate) base_intensity: f64,
    /// From -2 to 2: how far it breaks the pattern of what these people do.
    pub(crate) novelty: f64,
    pub(crate) evidence_signals: Vec<String>,
    /// Out of 10.
    pub(crate) evidence_strength: f64,
    /// The context labels it rests on.
    pub(crate) evidence_contexts: Vec<String>,
}

/// Why the gate turned a proposal down: the first of its conditions the proposal misses, in the
/// order `gate_episode` checks them, or that it is about no person (`publish_episode`'s own check).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GateRefusal {
    NotAboutAPerson,
    UnknownLens,
    NoTitle,
    NoSummary,
    NoEvidenceSignals,
    WeakEvidence,
    OffTheLeadingContexts,
    BelowIntensity,
}

impl GateRefusal {
    /// The word a log line names it by.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::NotAboutAPerson => "not_about_a_person",
            Self::UnknownLens => "unknown_lens",
            Self::NoTitle => "no_title",
            Self::NoSummary => "no_summary",
            Self::NoEvidenceSignals => "no_evidence_signals",
            Self::WeakEvidence => "weak_evidence",
            Self::OffTheLeadingContexts => "off_the_leading_contexts",
            Self::BelowIntensity => "below_intensity",
        }
    }
}

/// A proposal's final intensity, out of 10, rounded: its base, less a point for every two live
/// episodes of its lens these people already have (`habit`, up to five points), plus its novelty
/// (up to two either way). Computed whether or not the rest of the gate holds, so that a refusal
/// can show it.
pub(crate) fn final_intensity(proposal: &EpisodeProposal, habit: u32) -> f64 {
    let habit_penalty = f64::from((habit / 2).min(5));
    (proposal.base_intensity.clamp(0.0, 10.0) - habit_penalty + proposal.novelty.clamp(-2.0, 2.0))
        .round()
        .clamp(0.0, 10.0)
}

/// The deterministic gate, the lab's (§12.4). An episode is made at `EPISODE_MIN_INTENSITY` or more
/// of final intensity (`final_intensity`), with a known lens, a title and a summary, evidence of
/// `EPISODE_MIN_EVIDENCE` or more, and one of its contexts among the communication's two leading
/// ones (`leading`). Otherwise the first condition it misses says why.
pub(crate) fn gate_episode(
    proposal: &EpisodeProposal,
    leading: &[String],
    habit: u32,
) -> Result<f64, GateRefusal> {
    if !EPISODE_LENSES.contains(&proposal.lens.as_str()) {
        return Err(GateRefusal::UnknownLens);
    }
    if proposal.title.trim().is_empty() {
        return Err(GateRefusal::NoTitle);
    }
    if proposal.summary.trim().is_empty() {
        return Err(GateRefusal::NoSummary);
    }
    if proposal
        .evidence_signals
        .iter()
        .all(|signal| signal.trim().is_empty())
    {
        return Err(GateRefusal::NoEvidenceSignals);
    }
    if proposal.evidence_strength < EPISODE_MIN_EVIDENCE {
        return Err(GateRefusal::WeakEvidence);
    }
    let anchored = proposal.evidence_contexts.iter().any(|context| {
        leading
            .iter()
            .any(|label| label.trim().eq_ignore_ascii_case(context.trim()))
    });
    if !anchored {
        return Err(GateRefusal::OffTheLeadingContexts);
    }
    let intensity = final_intensity(proposal, habit);
    if intensity < EPISODE_MIN_INTENSITY {
        return Err(GateRefusal::BelowIntensity);
    }
    Ok(intensity)
}

/// The episode a proposal that passed the gate makes.
pub(crate) fn episode_of(
    episode_id: String,
    proposal: &EpisodeProposal,
    intensity: f64,
    born_at_ms: i64,
    people: Vec<String>,
    origin: EpisodeOrigin,
) -> Episode {
    let state = if EPISODE_STATES.contains(&proposal.state.as_str()) {
        proposal.state.clone()
    } else {
        "inconclusive".to_string()
    };
    Episode {
        episode_id,
        lens: proposal.lens.clone(),
        title: cut(&proposal.title, EPISODE_TITLE_MAX_CHARS),
        summary: cut(&proposal.summary, EPISODE_SUMMARY_MAX_CHARS),
        state,
        intensity,
        born_at_ms,
        people,
        origin,
    }
}

/// The ILKs of the people an episode is about: each once, in their order, at most
/// `EPISODE_MAX_PEOPLE`.
pub(crate) fn people_of<'a>(ilks: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut people = distinct_ilks(ilks);
    people.truncate(EPISODE_MAX_PEOPLE);
    people
}

/// ILKs trimmed, each once whatever its case, in their order.
pub(crate) fn distinct_ilks<'a>(ilks: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    ilks.into_iter()
        .map(str::trim)
        .filter(|ilk| !ilk.is_empty() && seen.insert(ilk.to_ascii_lowercase()))
        .map(str::to_string)
        .collect()
}

/// The episodes this cognition keeps in memory, the same its file keeps (the file is read at start).
#[derive(Debug, Default)]
pub(crate) struct EpisodeBook {
    by_id: HashMap<String, Episode>,
}

impl EpisodeBook {
    /// Keeps an episode it does not have yet; false when it had it.
    pub(crate) fn insert(&mut self, episode: Episode) -> bool {
        if self.by_id.contains_key(&episode.episode_id) {
            return false;
        }
        self.by_id.insert(episode.episode_id.clone(), episode);
        true
    }

    /// Lets go the episodes faded by `now_ms`; returns how many.
    pub(crate) fn sweep(&mut self, now_ms: i64) -> usize {
        let before = self.by_id.len();
        self.by_id
            .retain(|_, episode| episode.fades_at_ms() > now_ms);
        before - self.by_id.len()
    }

    pub(crate) fn len(&self) -> usize {
        self.by_id.len()
    }

    /// The habit: how many live episodes of `lens` are about exactly these people.
    pub(crate) fn habit(&self, lens: &str, people: &[String], now_ms: i64) -> u32 {
        let wanted = lowercase_set(people);
        self.by_id
            .values()
            .filter(|episode| {
                episode.lens == lens
                    && episode.fades_at_ms() > now_ms
                    && lowercase_set(&episode.people) == wanted
            })
            .count() as u32
    }

    /// The habit of each lens for these people, for the narrator.
    pub(crate) fn habits(&self, people: &[String], now_ms: i64) -> Vec<(&'static str, u32)> {
        EPISODE_LENSES
            .iter()
            .map(|lens| (*lens, self.habit(lens, people, now_ms)))
            .collect()
    }

    /// The news for a conversation whose people are `people`: the live episodes about any of them,
    /// the strongest first, then the newest, at most `PACKAGE_MAX_NEWS`, each person as `party`
    /// names them. Also how many more there were.
    pub(crate) fn news_for(
        &self,
        people: &[String],
        now_ms: i64,
        party: impl Fn(&str) -> MemoryTurnParty,
    ) -> (Vec<NewsSummary>, u32) {
        let wanted = lowercase_set(people);
        if wanted.is_empty() {
            return (Vec::new(), 0);
        }
        let mut found: Vec<(f64, &Episode)> = self
            .by_id
            .values()
            .filter(|episode| {
                episode
                    .people
                    .iter()
                    .any(|ilk| wanted.contains(&ilk.to_ascii_lowercase()))
            })
            .map(|episode| (episode.weight_at(now_ms), episode))
            .filter(|(weight, _)| *weight >= EPISODE_FADED_BELOW)
            .collect();
        found.sort_by(|(left_weight, left), (right_weight, right)| {
            right_weight
                .total_cmp(left_weight)
                .then_with(|| right.born_at_ms.cmp(&left.born_at_ms))
                .then_with(|| left.episode_id.cmp(&right.episode_id))
        });
        let dropped = found.len().saturating_sub(PACKAGE_MAX_NEWS) as u32;
        let news = found
            .into_iter()
            .take(PACKAGE_MAX_NEWS)
            .map(|(weight, episode)| NewsSummary {
                episode_id: episode.episode_id.clone(),
                lens: episode.lens.clone(),
                title: episode.title.clone(),
                summary: episode.summary.clone(),
                state: episode.state.clone(),
                people: episode.people.iter().map(|ilk| party(ilk)).collect(),
                born_at: chrono::DateTime::from_timestamp_millis(episode.born_at_ms)
                    .map(|at| at.to_rfc3339())
                    .unwrap_or_default(),
                weight: (weight * 100.0).round() / 100.0,
            })
            .collect();
        (news, dropped)
    }
}

fn lowercase_set(ilks: &[String]) -> BTreeSet<String> {
    ilks.iter()
        .map(|ilk| ilk.trim().to_ascii_lowercase())
        .filter(|ilk| !ilk.is_empty())
        .collect()
}

fn cut(text: &str, max_chars: usize) -> String {
    text.trim()
        .chars()
        .take(max_chars)
        .collect::<String>()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 24 * 60 * 60 * 1000;
    const BORN: i64 = 1_800_000_000_000;

    fn proposal() -> EpisodeProposal {
        EpisodeProposal {
            lens: "friction".to_string(),
            title: "Argument over the credit note".to_string(),
            summary: "Ana and Juan argued over the credit note's delay; it is pending.".to_string(),
            state: "pending".to_string(),
            base_intensity: 8.0,
            novelty: 0.0,
            evidence_signals: vec!["escalation".to_string()],
            evidence_strength: 9.0,
            evidence_contexts: vec!["Credit Note".to_string()],
        }
    }

    fn episode(id: &str, lens: &str, intensity: f64, born_at_ms: i64, people: &[&str]) -> Episode {
        Episode {
            episode_id: id.to_string(),
            lens: lens.to_string(),
            title: format!("{lens} {id}"),
            summary: format!("What happened in {id}."),
            state: "pending".to_string(),
            intensity,
            born_at_ms,
            people: people.iter().map(|ilk| ilk.to_string()).collect(),
            origin: EpisodeOrigin {
                thread_id: "thread:1".to_string(),
                node: "SY.cognition@worker1".to_string(),
            },
        }
    }

    fn named(ilk: &str) -> MemoryTurnParty {
        MemoryTurnParty {
            ilk: Some(ilk.to_string()),
            name: None,
            kind: Some("person".to_string()),
        }
    }

    #[test]
    fn the_gate_is_the_labs() {
        let leading = vec!["credit note".to_string(), "billing".to_string()];
        assert_eq!(gate_episode(&proposal(), &leading, 0), Ok(8.0));
        // The habit takes a point off for every two live episodes of the lens, up to five.
        assert_eq!(gate_episode(&proposal(), &leading, 2), Ok(7.0));
        assert_eq!(
            gate_episode(&proposal(), &leading, 4),
            Err(GateRefusal::BelowIntensity)
        );
        // The novelty adds up to two.
        let novel = EpisodeProposal {
            novelty: 5.0,
            ..proposal()
        };
        assert_eq!(
            gate_episode(&novel, &leading, 20),
            Err(GateRefusal::BelowIntensity)
        );
        assert_eq!(gate_episode(&novel, &leading, 4), Ok(8.0));
        // Each other condition alone refuses it, and says which.
        for (refused, why) in [
            (
                EpisodeProposal {
                    lens: "joy".to_string(),
                    ..proposal()
                },
                GateRefusal::UnknownLens,
            ),
            (
                EpisodeProposal {
                    title: " ".to_string(),
                    ..proposal()
                },
                GateRefusal::NoTitle,
            ),
            (
                EpisodeProposal {
                    summary: " ".to_string(),
                    ..proposal()
                },
                GateRefusal::NoSummary,
            ),
            (
                EpisodeProposal {
                    evidence_signals: vec![" ".to_string()],
                    ..proposal()
                },
                GateRefusal::NoEvidenceSignals,
            ),
            (
                EpisodeProposal {
                    evidence_strength: 7.9,
                    ..proposal()
                },
                GateRefusal::WeakEvidence,
            ),
            (
                EpisodeProposal {
                    evidence_contexts: vec!["refunds".to_string()],
                    ..proposal()
                },
                GateRefusal::OffTheLeadingContexts,
            ),
            (
                EpisodeProposal {
                    base_intensity: 6.4,
                    ..proposal()
                },
                GateRefusal::BelowIntensity,
            ),
        ] {
            assert_eq!(gate_episode(&refused, &leading, 0), Err(why), "{refused:?}");
        }
        // With several missed, the first in the gate's order says why.
        let weak_and_unknown = EpisodeProposal {
            lens: "joy".to_string(),
            evidence_strength: 1.0,
            ..proposal()
        };
        assert_eq!(
            gate_episode(&weak_and_unknown, &leading, 0),
            Err(GateRefusal::UnknownLens)
        );
    }

    #[test]
    fn a_refused_proposal_still_has_its_final_intensity() {
        // What a refusal's log line shows: the base, less the habit, plus the novelty.
        assert_eq!(final_intensity(&proposal(), 0), 8.0);
        assert_eq!(final_intensity(&proposal(), 4), 6.0);
        let off_the_contexts = EpisodeProposal {
            evidence_contexts: vec!["refunds".to_string()],
            novelty: -0.6,
            ..proposal()
        };
        assert_eq!(final_intensity(&off_the_contexts, 0), 7.0);
        assert_eq!(
            GateRefusal::OffTheLeadingContexts.as_str(),
            "off_the_leading_contexts"
        );
    }

    #[test]
    fn an_episode_halves_every_seven_days_and_leaves_under_two() {
        let made = episode("e1", "friction", 8.0, BORN, &["ilk:ana"]);
        assert!((made.weight_at(BORN) - 8.0).abs() < 1e-9);
        assert!((made.weight_at(BORN + 7 * DAY_MS) - 4.0).abs() < 1e-9);
        assert!((made.weight_at(BORN + 14 * DAY_MS) - 2.0).abs() < 1e-9);
        assert_eq!(made.fades_at_ms(), BORN + 14 * DAY_MS);

        let mut book = EpisodeBook::default();
        assert!(book.insert(made.clone()));
        assert!(!book.insert(made), "an episode is kept once");
        assert_eq!(book.sweep(BORN + 14 * DAY_MS - 1), 0);
        assert_eq!(book.sweep(BORN + 14 * DAY_MS), 1);
        assert_eq!(book.len(), 0);
    }

    #[test]
    fn the_news_are_those_of_the_conversations_people_the_strongest_first() {
        let mut book = EpisodeBook::default();
        let now = BORN + 3 * DAY_MS;
        book.insert(episode(
            "old",
            "friction",
            10.0,
            BORN - 20 * DAY_MS,
            &["ilk:ana"],
        ));
        book.insert(episode(
            "new",
            "appreciation",
            7.0,
            BORN,
            &["ILK:Ana", "ilk:juan"],
        ));
        book.insert(episode(
            "strong",
            "friction",
            9.0,
            BORN,
            &["ilk:juan", "ilk:pedro"],
        ));
        book.insert(episode("other", "shame", 9.0, BORN, &["ilk:pedro"]));
        for n in 0..5 {
            book.insert(episode(
                &format!("more{n}"),
                "discomfort",
                7.0,
                BORN - DAY_MS,
                &["ilk:ana"],
            ));
        }
        let (news, dropped) =
            book.news_for(&["ilk:ana".to_string(), "ilk:juan".to_string()], now, named);
        let ids: Vec<&str> = news.iter().map(|news| news.episode_id.as_str()).collect();
        assert_eq!(ids, ["strong", "new", "more0", "more1", "more2"]);
        assert_eq!(
            dropped, 2,
            "two more of the people's, past the five a package carries"
        );
        assert_eq!(news[0].people[1].ilk.as_deref(), Some("ilk:pedro"));
        assert_eq!(news[0].born_at, "2027-01-15T08:00:00+00:00");
        assert!((news[0].weight - 6.69).abs() < 1e-9);
        // The faded one is not news, and no one's people bring none.
        assert!(book
            .news_for(&["ilk:ana".to_string()], now, named)
            .0
            .iter()
            .all(|news| news.episode_id != "old"));
        assert!(book.news_for(&[], now, named).0.is_empty());
    }

    #[test]
    fn the_habit_counts_a_lens_among_the_same_people() {
        let mut book = EpisodeBook::default();
        book.insert(episode(
            "a",
            "friction",
            8.0,
            BORN,
            &["ilk:ana", "ilk:juan"],
        ));
        book.insert(episode(
            "b",
            "friction",
            8.0,
            BORN,
            &["ilk:juan", "ILK:ANA"],
        ));
        book.insert(episode("c", "friction", 8.0, BORN, &["ilk:ana"]));
        book.insert(episode("d", "shame", 8.0, BORN, &["ilk:ana", "ilk:juan"]));
        let pair = ["ilk:ana".to_string(), "ilk:juan".to_string()];
        assert_eq!(book.habit("friction", &pair, BORN), 2);
        assert_eq!(
            book.habit("friction", &pair, BORN + 15 * DAY_MS),
            0,
            "faded ones are no habit"
        );
        assert_eq!(
            book.habits(&pair, BORN),
            [
                ("friction", 2),
                ("discomfort", 0),
                ("appreciation", 0),
                ("shame", 1)
            ]
        );
    }

    #[test]
    fn an_episode_from_another_cognition_is_checked() {
        let good = episode(
            "e1",
            "friction",
            8.0,
            BORN,
            &[" ilk:ana ", "ilk:ana", "ilk:juan"],
        );
        let checked = good.clone().checked().expect("a good one");
        assert_eq!(checked.people, ["ilk:ana", "ilk:juan"]);
        for bad in [
            Episode {
                lens: "joy".to_string(),
                ..good.clone()
            },
            Episode {
                state: "open".to_string(),
                ..good.clone()
            },
            Episode {
                intensity: 6.0,
                ..good.clone()
            },
            Episode {
                intensity: 11.0,
                ..good.clone()
            },
            Episode {
                people: vec![" ".to_string()],
                ..good.clone()
            },
            Episode {
                summary: String::new(),
                ..good.clone()
            },
            Episode {
                episode_id: " ".to_string(),
                ..good.clone()
            },
        ] {
            assert!(bad.checked().is_none());
        }
        let long = Episode {
            summary: "x".repeat(2_000),
            ..good
        };
        assert_eq!(
            long.checked().expect("cut").summary.len(),
            EPISODE_SUMMARY_MAX_CHARS
        );
    }

    #[test]
    fn a_proposal_makes_an_episode_with_a_known_state() {
        let origin = EpisodeOrigin {
            thread_id: "thread:1".to_string(),
            node: "SY.cognition@worker1".to_string(),
        };
        let made = episode_of(
            "e1".to_string(),
            &EpisodeProposal {
                state: "open".to_string(),
                ..proposal()
            },
            8.0,
            BORN,
            people_of(["ilk:ana", "ilk:juan"]),
            origin,
        );
        assert_eq!(made.state, "inconclusive");
        assert_eq!(made.people, ["ilk:ana", "ilk:juan"]);
        assert!(made.clone().checked().is_some());
    }
}
