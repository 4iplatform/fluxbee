//! When an AI node says it turned messages away, or that the router turned its own away: at once,
//! then at most once in `REFUSAL_LOG_INTERVAL`, with how many since its last line, as the router
//! does for a full link (FINDINGS A-222). Under load these come at the rate messages arrive, and a
//! line for each would flood the journal (FINDINGS A-257).

use std::time::Duration;

use tokio::time::Instant;

/// The least time between two lines of one `RefusalLog`.
pub(crate) const REFUSAL_LOG_INTERVAL: Duration = Duration::from_secs(10);

/// Counts what one kind of refusal turned away, and says when its line is due: the first one at
/// once, then at most once every `REFUSAL_LOG_INTERVAL`.
#[derive(Debug, Default)]
pub(crate) struct RefusalLog {
    logged_at: Option<Instant>,
    unlogged: u64,
}

impl RefusalLog {
    /// Counts one; when its line is due, how many to name in it (this one and those since the
    /// last).
    pub(crate) fn note(&mut self, now: Instant) -> Option<u64> {
        self.unlogged += 1;
        self.flush(now)
    }

    /// How many were counted and not named yet, once their line is due: the end of a burst, which
    /// no later refusal would name. None when there are none, or the last line is too recent.
    pub(crate) fn flush(&mut self, now: Instant) -> Option<u64> {
        if self.unlogged == 0
            || self
                .logged_at
                .is_some_and(|at| now.duration_since(at) < REFUSAL_LOG_INTERVAL)
        {
            return None;
        }
        self.logged_at = Some(now);
        Some(std::mem::take(&mut self.unlogged))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_is_named_at_once_then_once_an_interval() {
        let mut log = RefusalLog::default();
        let start = Instant::now();
        assert_eq!(log.note(start), Some(1));
        assert_eq!(log.note(start + Duration::from_secs(1)), None);
        assert_eq!(log.note(start + Duration::from_secs(9)), None);
        assert_eq!(log.note(start + REFUSAL_LOG_INTERVAL), Some(3));
        assert_eq!(
            log.note(start + REFUSAL_LOG_INTERVAL + Duration::from_secs(1)),
            None
        );
    }

    /// The end of a burst is named once its line is due, though nothing more is turned away.
    #[test]
    fn the_end_of_a_burst_is_named_once_its_line_is_due() {
        let mut log = RefusalLog::default();
        let start = Instant::now();
        assert_eq!(log.flush(start), None, "nothing to name");
        assert_eq!(log.note(start), Some(1));
        assert_eq!(log.note(start + Duration::from_secs(2)), None);
        assert_eq!(log.note(start + Duration::from_secs(3)), None);
        assert_eq!(log.flush(start + Duration::from_secs(9)), None, "too soon");
        assert_eq!(log.flush(start + REFUSAL_LOG_INTERVAL), Some(2));
        assert_eq!(log.flush(start + REFUSAL_LOG_INTERVAL * 3), None, "named");
        // The next refusal after a quiet interval is named at once again.
        assert_eq!(log.note(start + REFUSAL_LOG_INTERVAL * 3), Some(1));
    }
}
