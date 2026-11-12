//! The router's journal in its tests: what a test's thread logs at `INFO` and above, rt-gateway's
//! default level (`JSR_LOG_LEVEL`), as plain text with no time.
//!
//! One global subscriber, installed by the first capture, formats each event at those levels and
//! writes it to the journal of the thread it happened on, if that thread captures one. A
//! subscriber set as one thread's default (`set_default`, `with_default`) could lose a line: while
//! it was the only one registered, tracing asked the thread that reached a callsite first whether
//! the callsite was wanted (tracing-core's `has_just_one`), and another test's thread, with no
//! subscriber, cached "never" for every thread (A-269's review).

use std::cell::RefCell;
use std::io;
use std::sync::{Arc, Mutex, OnceLock};

/// What one thread logged while a capture of it lived.
#[derive(Clone, Default)]
pub(super) struct TestJournal(Arc<Mutex<Vec<u8>>>);

/// Ends a capture when dropped: the thread's journal goes back to the one before it, if any.
pub(super) struct Capture(Option<TestJournal>);

thread_local! {
    static CAPTURED: RefCell<Option<TestJournal>> = const { RefCell::new(None) };
}

/// The global subscriber's writer: the journal of the thread the event happened on.
struct ThreadJournal;

impl io::Write for ThreadJournal {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        CAPTURED.with(|journal| {
            if let Some(journal) = journal.borrow().as_ref() {
                journal
                    .0
                    .lock()
                    .expect("the journal")
                    .extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl TestJournal {
    /// Captures what this thread logs until the guard drops. A `#[tokio::test]` runs its spawned
    /// tasks on its own thread, so a connection's lines are in it.
    pub(super) fn capture(&self) -> Capture {
        static GLOBAL: OnceLock<()> = OnceLock::new();
        GLOBAL.get_or_init(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_writer(|| ThreadJournal)
                .with_ansi(false)
                .without_time()
                .finish();
            tracing::subscriber::set_global_default(subscriber)
                .expect("the test journal's subscriber is the only global one");
        });
        // A callsite reached before the subscriber was installed keeps no "never".
        tracing::callsite::rebuild_interest_cache();
        Capture(CAPTURED.with(|journal| journal.replace(Some(self.clone()))))
    }

    pub(super) fn text(&self) -> String {
        String::from_utf8(self.0.lock().expect("the journal").clone()).expect("text")
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let before = self.0.take();
        CAPTURED.with(|journal| *journal.borrow_mut() = before);
    }
}

/// A-269's review: a line is kept although another thread, capturing nothing, reached its callsite
/// first. With a capture set as the thread's default and no other subscriber registered, that
/// thread cached the callsite as unwanted and the line was lost. Lines and journals stay apart:
/// the other thread's line is in no journal, and one captured inside another goes to the inner
/// one alone.
#[test]
fn a_line_whose_callsite_another_thread_reached_first_is_kept() {
    fn line(n: u32) {
        tracing::warn!(n, "the test journal's own line");
    }
    let journal = TestJournal::default();
    let inner = TestJournal::default();
    {
        let _capture = journal.capture();
        std::thread::spawn(|| line(1))
            .join()
            .expect("the other thread");
        line(2);
        {
            let _inner = inner.capture();
            line(3);
        }
        line(4);
    }
    line(5);
    let text = journal.text();
    for (n, kept) in [(1, false), (2, true), (3, false), (4, true), (5, false)] {
        assert_eq!(
            text.contains(&format!("the test journal's own line n={n}")),
            kept,
            "{n}: {text}"
        );
    }
    assert_eq!(inner.text().lines().count(), 1, "{}", inner.text());
    assert!(inner.text().contains("n=3"));
}
