//! What is open on each exposure, and its cut (docs/io-web-v2.md §13.4;
//! docs/io-web-v2-stage-2-2.md §4.7, §4.8).
//!
//! - **What is open:** every forwarded request until its answer ends, every upgraded connection
//!   until it closes. Each holds a `Registration`, which counts it and carries the exposure's cut
//!   signal.
//! - **The cut:** once the exposure is left without access (a push takes away its last live
//!   enablement, or the exposure itself; or io.web's clock passes its last enablement's end), its
//!   `Gate` closes: every registration's signal fires at once, and nothing registers until a push
//!   gives the exposure access again.
//! - **No race:** a gate is closed only under the held set's write lock (a push, the clock), and a
//!   request registers under its read lock, in the same turn as its session check
//!   (`Exposures::admit`). A request is therefore registered before the cut, and cut with the
//!   rest, or checked after it, and refused.
//!
//! A gate lives as long as its exposure: a push carries it over to the same exposure (its id on
//! its host), as it carries the keys, so its counters run for the life of the process.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::watch;

/// Resolves once the exposure a request registered on is cut.
pub type Cut = Pin<Box<dyn Future<Output = ()> + Send>>;

/// One exposure's cut and counters.
pub struct Gate {
    /// Moved on by each cut: a registration is cut once the epoch is no longer the one it
    /// registered at. A gate opened again keeps its epoch, so nothing cut comes back.
    epoch: watch::Sender<u64>,
    /// The exposure is without access: what was open is cut, and nothing registers until a push
    /// opens it again. Written under the held set's write lock, read under its read lock.
    closed: AtomicBool,
    counts: Counts,
}

/// What a gate counts (§4.8): what is open now, and what ended how since io.web started.
#[derive(Default)]
struct Counts {
    waiting: AtomicU64,
    streams: AtomicU64,
    websockets: AtomicU64,
    /// By the answer's class: 1xx (a WebSocket's 101), 2xx, 3xx, 4xx and 5xx.
    proxied: [AtomicU64; 5],
    refused_401: AtomicU64,
    refused_403: AtomicU64,
    upstream_failures: AtomicU64,
    cut: AtomicU64,
    sessions_moved: AtomicU64,
}

/// What is open of one request: it waits for its upstream's head, its answer's body streams, or
/// its connection is a tunnel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Waiting,
    Stream,
    WebSocket,
}

/// What a gate held open when it closed, for the cut's log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Held {
    pub waiting: u64,
    pub streams: u64,
    pub websockets: u64,
}

impl Held {
    pub fn total(&self) -> u64 {
        self.waiting + self.streams + self.websockets
    }
}

impl Gate {
    /// A gate open, or closed for an exposure without access.
    pub fn new(open: bool) -> Arc<Self> {
        Arc::new(Self {
            epoch: watch::Sender::new(0),
            closed: AtomicBool::new(!open),
            counts: Counts::default(),
        })
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Relaxed)
    }

    /// Cuts everything registered, at once, and closes the gate; what it held open then, or
    /// `None` when it was closed already. Under the held set's write lock.
    pub fn close(&self) -> Option<Held> {
        if self.closed.swap(true, Ordering::Relaxed) {
            return None;
        }
        let held = self.held();
        self.epoch.send_modify(|epoch| *epoch += 1);
        Some(held)
    }

    /// Lets requests register again: a push gave the exposure access. Under the held set's write
    /// lock.
    pub fn open(&self) {
        self.closed.store(false, Ordering::Relaxed);
    }

    /// A request on this gate's exposure, waiting for its upstream; `None` while the gate is
    /// closed. Under the held set's read lock.
    pub fn register(self: &Arc<Self>) -> Option<Registration> {
        if self.is_closed() {
            return None;
        }
        self.counts.waiting.fetch_add(1, Ordering::Relaxed);
        Some(Registration {
            gate: Arc::clone(self),
            epoch: *self.epoch.borrow(),
            class: Class::Waiting,
        })
    }

    fn held(&self) -> Held {
        Held {
            waiting: self.counts.waiting.load(Ordering::Relaxed),
            streams: self.counts.streams.load(Ordering::Relaxed),
            websockets: self.counts.websockets.load(Ordering::Relaxed),
        }
    }

    /// An upstream's answer of `status`.
    pub fn count_answer(&self, status: u16) {
        let class = usize::from(status / 100).clamp(1, 5) - 1;
        self.counts.proxied[class].fetch_add(1, Ordering::Relaxed);
    }

    /// A refusal: 401 or 403.
    pub fn count_refused(&self, status: u16) {
        let count = if status == 403 {
            &self.counts.refused_403
        } else {
            &self.counts.refused_401
        };
        count.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count_upstream_failure(&self) {
        self.counts
            .upstream_failures
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn count_cut(&self) {
        self.counts.cut.fetch_add(1, Ordering::Relaxed);
    }

    pub fn count_session_moved(&self) {
        self.counts.sessions_moved.fetch_add(1, Ordering::Relaxed);
    }

    /// Its part of `extensions.web.exposures.per_exposure` (§4.8).
    pub fn view(&self) -> Value {
        let load = |count: &AtomicU64| count.load(Ordering::Relaxed);
        let held = self.held();
        let proxied = &self.counts.proxied;
        json!({
            "open": {
                "waiting": held.waiting,
                "streams": held.streams,
                "websockets": held.websockets,
            },
            "proxied": {
                "1xx": load(&proxied[0]),
                "2xx": load(&proxied[1]),
                "3xx": load(&proxied[2]),
                "4xx": load(&proxied[3]),
                "5xx": load(&proxied[4]),
            },
            "refused": {
                "401": load(&self.counts.refused_401),
                "403": load(&self.counts.refused_403),
            },
            "upstream_failures": load(&self.counts.upstream_failures),
            "cut": load(&self.counts.cut),
            "sessions_moved": load(&self.counts.sessions_moved),
        })
    }
}

/// One request open on an exposure, counted in its class until it is dropped.
pub struct Registration {
    gate: Arc<Gate>,
    epoch: u64,
    class: Class,
}

impl Registration {
    /// Resolves once the exposure is cut, at once if it was cut already.
    pub fn cut(&self) -> Cut {
        let mut epoch = self.gate.epoch.subscribe();
        let registered = self.epoch;
        Box::pin(async move {
            // The gate holds the sender as long as this registration lives; a sender gone would
            // be no exposure at all, a cut too.
            let _ = epoch.wait_for(|now| *now != registered).await;
        })
    }

    /// Moves it to `class`: its answer streams, or its connection became a tunnel.
    pub fn now(&mut self, class: Class) {
        if class != self.class {
            self.counter(self.class).fetch_sub(1, Ordering::Relaxed);
            self.counter(class).fetch_add(1, Ordering::Relaxed);
            self.class = class;
        }
    }

    fn counter(&self, class: Class) -> &AtomicU64 {
        match class {
            Class::Waiting => &self.gate.counts.waiting,
            Class::Stream => &self.gate.counts.streams,
            Class::WebSocket => &self.gate.counts.websockets,
        }
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        self.counter(self.class).fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    /// A close cuts every registration at once, the ones made before it, and refuses new ones
    /// until the gate opens again; a registration made after it opens is not cut by the close
    /// before.
    #[tokio::test]
    async fn a_close_cuts_what_registered_and_refuses_the_rest() {
        let gate = Gate::new(true);
        let mut stream = gate.register().unwrap();
        stream.now(Class::Stream);
        let waiting = gate.register().unwrap();
        assert_eq!(
            gate.view()["open"],
            json!({"waiting": 1, "streams": 1, "websockets": 0})
        );
        let (cut_stream, cut_waiting) = (stream.cut(), waiting.cut());

        assert_eq!(
            gate.close(),
            Some(Held {
                waiting: 1,
                streams: 1,
                websockets: 0
            })
        );
        assert_eq!(gate.close(), None, "closed already");
        timeout(Duration::from_secs(1), cut_stream).await.unwrap();
        timeout(Duration::from_secs(1), cut_waiting).await.unwrap();
        // A signal asked for after the close fires at once too.
        timeout(Duration::from_secs(1), stream.cut()).await.unwrap();
        assert!(gate.register().is_none());
        drop((stream, waiting));
        assert_eq!(
            gate.view()["open"],
            json!({"waiting": 0, "streams": 0, "websockets": 0})
        );

        gate.open();
        let after = gate.register().expect("open again");
        assert!(
            timeout(Duration::from_millis(50), after.cut())
                .await
                .is_err(),
            "a registration after the gate opened is not cut"
        );
    }

    #[test]
    fn the_counts_follow_each_end() {
        let gate = Gate::new(false);
        assert!(gate.register().is_none(), "a closed gate");
        for status in [101, 200, 204, 302, 404, 502, 599] {
            gate.count_answer(status);
        }
        gate.count_refused(401);
        gate.count_refused(403);
        gate.count_upstream_failure();
        gate.count_cut();
        gate.count_session_moved();
        let view = gate.view();
        assert_eq!(
            view["proxied"],
            json!({"1xx": 1, "2xx": 2, "3xx": 1, "4xx": 1, "5xx": 2})
        );
        assert_eq!(view["refused"], json!({"401": 1, "403": 1}));
        assert_eq!(
            (
                &view["upstream_failures"],
                &view["cut"],
                &view["sessions_moved"]
            ),
            (&json!(1), &json!(1), &json!(1))
        );
    }
}
