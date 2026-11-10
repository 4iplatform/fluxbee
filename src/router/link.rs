//! The queue of frames for one link's writer: a node's, a WAN peer's or a sibling router's (FINDINGS
//! A-222).
//!
//! It is the only way a frame reaches a link, and it is bounded. Past `LINK_QUEUE_SENDERS_FRAMES`
//! frames or `LINK_QUEUE_SENDERS_BYTES` bytes waiting, a sender's message is refused
//! (`LinkSender::queue`), and the router answers its sender `LINK_BUSY`. The rest of the bound,
//! up to `LINK_QUEUE_MAX_FRAMES` and `LINK_QUEUE_MAX_BYTES`, is kept for the router's own frames
//! (its answers, LSAs, handshakes: `LinkSender::queue_own`), so senders that keep a link at their
//! bound never crowd them out; one of them is skipped only when the queue is full, and never
//! answered. A frame the queue took is never dropped to make room, and the link is never cut for
//! it: room comes back as its writer drains it. Until 0.1.129 the queue had no bound, and every
//! frame for a node that stopped reading without closing stayed in the router's memory.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use tokio::sync::mpsc;

use crate::shm::now_epoch_ms;

/// The most frames a link's queue holds for its writer, the router's own included.
pub(super) const LINK_QUEUE_MAX_FRAMES: usize = 1_024;
/// The most bytes a link's queue holds for its writer, the router's own frames included: 64 frames
/// at the socket's 128 KiB limit.
pub(super) const LINK_QUEUE_MAX_BYTES: usize = 8 * 1024 * 1024;
/// The frames a sender's messages may take; the other 64 are the router's own.
pub(super) const LINK_QUEUE_SENDERS_FRAMES: usize = LINK_QUEUE_MAX_FRAMES - 64;
/// The bytes a sender's messages may take; the other MiB is the router's own.
pub(super) const LINK_QUEUE_SENDERS_BYTES: usize = LINK_QUEUE_MAX_BYTES - 1024 * 1024;
/// At most one line per link and interval about the frames its queue did not take.
const LINK_FULL_LOG_INTERVAL_MS: u64 = 30_000;
/// Frames the links' queues did not take for being full, process-wide. Each line carries the total.
static LINK_FRAMES_REFUSED: AtomicU64 = AtomicU64::new(0);

/// Why a link's queue did not take a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LinkRefusal {
    /// The queue is at its bound: the link's writer is behind, its peer not reading.
    Full,
    /// The link's writer has stopped (a write error, the link's end): nothing reaches its peer.
    Gone,
}

/// What the senders of a link and its writer share.
struct LinkState {
    /// The kind of link, for the lines: `node`, `wan` or `irp`.
    kind: &'static str,
    /// Its peer, once known: the node's name, the hive, the sibling router's UUID.
    peer: OnceLock<String>,
    /// The frames and bytes waiting for the writer.
    frames: AtomicUsize,
    bytes: AtomicUsize,
    /// Since the last line: senders' messages refused, and the router's own frames skipped.
    refused: AtomicU64,
    skipped: AtomicU64,
    logged_at_ms: AtomicU64,
}

/// The sending side of a link's queue. Clones share the queue.
#[derive(Clone)]
pub(super) struct LinkSender {
    tx: mpsc::UnboundedSender<Vec<u8>>,
    state: Arc<LinkState>,
}

/// The writer's side of a link's queue.
pub(super) struct LinkReceiver {
    rx: mpsc::UnboundedReceiver<Vec<u8>>,
    state: Arc<LinkState>,
}

/// A new link's queue; `kind` names the link in the lines (`node`, `wan`, `irp`).
pub(super) fn link_queue(kind: &'static str) -> (LinkSender, LinkReceiver) {
    let (tx, rx) = mpsc::unbounded_channel();
    let state = Arc::new(LinkState {
        kind,
        peer: OnceLock::new(),
        frames: AtomicUsize::new(0),
        bytes: AtomicUsize::new(0),
        refused: AtomicU64::new(0),
        skipped: AtomicU64::new(0),
        logged_at_ms: AtomicU64::new(0),
    });
    (
        LinkSender {
            tx,
            state: Arc::clone(&state),
        },
        LinkReceiver { rx, state },
    )
}

impl LinkSender {
    /// Names the link's peer in the lines, once it is known.
    pub(super) fn set_peer(&self, peer: String) {
        let _ = self.state.peer.set(peer);
    }

    /// Queues a sender's message. `Full` when the queue holds the senders' bound: the caller
    /// answers the sender `LINK_BUSY`, and the line names the link and its depth, at most once
    /// every 30 s. `Gone` when the writer has stopped.
    pub(super) fn queue(&self, frame: Vec<u8>) -> Result<(), LinkRefusal> {
        self.take(frame, false)
    }

    /// Queues one of the router's own frames: an answer, an LSA, a handshake. These may use the
    /// whole bound; on a full queue the frame is skipped and counted in the link's line, and no one
    /// is answered. Whether it was queued.
    pub(super) fn queue_own(&self, frame: Vec<u8>) -> bool {
        self.take(frame, true).is_ok()
    }

    /// Completes once the link's writer has stopped.
    pub(super) async fn closed(&self) {
        self.tx.closed().await;
    }

    /// Whether both are senders of the same link's queue.
    pub(super) fn same_channel(&self, other: &Self) -> bool {
        self.tx.same_channel(&other.tx)
    }

    /// The frames and bytes waiting for the link's writer.
    pub(super) fn depth(&self) -> (usize, usize) {
        (
            self.state.frames.load(Ordering::Relaxed),
            self.state.bytes.load(Ordering::Relaxed),
        )
    }

    fn take(&self, frame: Vec<u8>, own: bool) -> Result<(), LinkRefusal> {
        if self.tx.is_closed() {
            return Err(LinkRefusal::Gone);
        }
        if !self.claim(frame.len(), own) {
            return Err(self.full(own));
        }
        self.tx.send(frame).map_err(|unsent| {
            self.release(unsent.0.len());
            LinkRefusal::Gone
        })
    }

    /// Takes a frame's place and bytes within the bound, or nothing.
    fn claim(&self, len: usize, own: bool) -> bool {
        let (max_frames, max_bytes) = if own {
            (LINK_QUEUE_MAX_FRAMES, LINK_QUEUE_MAX_BYTES)
        } else {
            (LINK_QUEUE_SENDERS_FRAMES, LINK_QUEUE_SENDERS_BYTES)
        };
        let state = &self.state;
        if state
            .frames
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |frames| {
                (frames < max_frames).then_some(frames + 1)
            })
            .is_err()
        {
            return false;
        }
        if state
            .bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |bytes| {
                bytes.checked_add(len).filter(|total| *total <= max_bytes)
            })
            .is_err()
        {
            state.frames.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn release(&self, len: usize) {
        self.state.frames.fetch_sub(1, Ordering::Relaxed);
        self.state.bytes.fetch_sub(len, Ordering::Relaxed);
    }

    /// Counts a frame the full queue did not take, and says so at most once per link every
    /// `LINK_FULL_LOG_INTERVAL_MS`, with the link, its depth and what it refused since.
    fn full(&self, own: bool) -> LinkRefusal {
        let state = &self.state;
        if own {
            state.skipped.fetch_add(1, Ordering::Relaxed);
        } else {
            state.refused.fetch_add(1, Ordering::Relaxed);
        }
        let refused_total = LINK_FRAMES_REFUSED.fetch_add(1, Ordering::Relaxed) + 1;
        let now = now_epoch_ms();
        let last = state.logged_at_ms.load(Ordering::Relaxed);
        if now.saturating_sub(last) >= LINK_FULL_LOG_INTERVAL_MS
            && state
                .logged_at_ms
                .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            let (queued_frames, queued_bytes) = self.depth();
            let refused = state.refused.swap(0, Ordering::Relaxed);
            let skipped = state.skipped.swap(0, Ordering::Relaxed);
            tracing::warn!(
                link = state.kind,
                peer = state.peer.get().map(String::as_str).unwrap_or("-"),
                queued_frames,
                queued_bytes,
                max_frames = LINK_QUEUE_MAX_FRAMES,
                max_bytes = LINK_QUEUE_MAX_BYTES,
                refused,
                skipped,
                refused_total,
                "router did not queue frames for a link whose queue is full (LINK_BUSY): senders \
                 answered, its own frames skipped; the link stays up"
            );
        }
        LinkRefusal::Full
    }
}

impl LinkReceiver {
    /// The next frame for the writer; its place in the bound is free again from here. None once
    /// every sender is gone.
    pub(super) async fn recv(&mut self) -> Option<Vec<u8>> {
        let frame = self.rx.recv().await?;
        self.state.frames.fetch_sub(1, Ordering::Relaxed);
        self.state.bytes.fetch_sub(frame.len(), Ordering::Relaxed);
        Some(frame)
    }

    /// The next frame if one is waiting, as `recv` takes it.
    #[cfg(test)]
    pub(super) fn try_recv(&mut self) -> Result<Vec<u8>, mpsc::error::TryRecvError> {
        let frame = self.rx.try_recv()?;
        self.state.frames.fetch_sub(1, Ordering::Relaxed);
        self.state.bytes.fetch_sub(frame.len(), Ordering::Relaxed);
        Ok(frame)
    }

    /// The kind of link, for the writer's lines.
    pub(super) fn kind(&self) -> &'static str {
        self.state.kind
    }

    /// The link's peer, for the writer's lines; `-` until it is known.
    pub(super) fn peer(&self) -> &str {
        self.state.peer.get().map(String::as_str).unwrap_or("-")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Queues small frames until the queue refuses one; how many it took.
    fn fill_with(link: &LinkSender, frame_len: usize) -> usize {
        let mut taken = 0;
        while link.queue(vec![b'x'; frame_len]).is_ok() {
            taken += 1;
            assert!(taken <= LINK_QUEUE_MAX_FRAMES, "the queue has a bound");
        }
        taken
    }

    /// A link whose writer does not read takes senders' frames up to their bound, then refuses
    /// each new one; the frames it took stay, in order, and once the writer takes one, one more
    /// fits.
    #[test]
    fn a_queue_refuses_a_senders_frame_at_its_bound_until_its_writer_drains_it() {
        let (link, mut writer) = link_queue("node");
        for n in 0..LINK_QUEUE_SENDERS_FRAMES {
            link.queue(n.to_string().into_bytes()).expect("room");
        }
        assert_eq!(link.queue(b"one more".to_vec()), Err(LinkRefusal::Full));
        assert_eq!(link.queue(b"and another".to_vec()), Err(LinkRefusal::Full));
        assert_eq!(link.depth().0, LINK_QUEUE_SENDERS_FRAMES);

        assert_eq!(writer.try_recv().expect("the first"), b"0".to_vec());
        link.queue(b"room again".to_vec()).expect("room again");
        assert_eq!(link.queue(b"full again".to_vec()), Err(LinkRefusal::Full));
        for n in 1..LINK_QUEUE_SENDERS_FRAMES {
            assert_eq!(writer.try_recv().expect("kept"), n.to_string().into_bytes());
        }
        assert_eq!(writer.try_recv().expect("the last"), b"room again".to_vec());
        assert!(writer.try_recv().is_err(), "nothing refused was queued");
        assert_eq!(link.depth(), (0, 0));
    }

    /// Large frames reach the byte budget long before the frame bound: the queue never holds more
    /// than its bytes, and a frame its bytes refused leaves its place free.
    #[test]
    fn a_queue_of_large_frames_stays_within_its_bytes() {
        const FRAME: usize = 100 * 1024;
        let (link, mut writer) = link_queue("wan");
        let taken = fill_with(&link, FRAME);
        assert_eq!(taken, LINK_QUEUE_SENDERS_BYTES / FRAME);
        let (frames, bytes) = link.depth();
        assert_eq!(frames, taken);
        assert!(bytes <= LINK_QUEUE_SENDERS_BYTES, "{bytes}");
        // The bytes refused it, not its place: a small frame still fits.
        link.queue(vec![b'x'; 1024]).expect("a small frame");

        writer.try_recv().expect("the writer takes one");
        link.queue(vec![b'x'; FRAME]).expect("its bytes free again");
        assert_eq!(link.queue(vec![b'x'; FRAME]), Err(LinkRefusal::Full));
        assert!(link.depth().1 <= LINK_QUEUE_SENDERS_BYTES);
    }

    /// The router's own frames keep the rest of the bound: they still go on a queue the senders
    /// filled, and are skipped only once it is full.
    #[test]
    fn the_routers_own_frames_have_the_rest_of_the_bound() {
        let (link, _writer) = link_queue("wan");
        fill_with(&link, 16);
        assert!(
            link.queue_own(b"an LSA".to_vec()),
            "a sender's bound is not the queue's"
        );
        let own = LINK_QUEUE_MAX_FRAMES - LINK_QUEUE_SENDERS_FRAMES;
        for _ in 1..own {
            assert!(link.queue_own(b"an answer".to_vec()));
        }
        assert!(
            !link.queue_own(b"skipped".to_vec()),
            "skipped once the queue is full"
        );
        assert_eq!(link.depth().0, LINK_QUEUE_MAX_FRAMES);
        assert_eq!(link.queue(b"refused".to_vec()), Err(LinkRefusal::Full));

        // Bytes too: one frame that would take the queue past its bytes is skipped.
        let (link, _writer) = link_queue("wan");
        fill_with(&link, 128 * 1024);
        assert!(link.queue_own(vec![b'x'; 1024 * 1024 - 64 * 1024]));
        assert!(!link.queue_own(vec![b'x'; 128 * 1024]));
        assert!(link.depth().1 <= LINK_QUEUE_MAX_BYTES);
    }

    /// A link whose writer has stopped takes nothing, full or not, and says it is gone; `closed`
    /// completes.
    #[tokio::test]
    async fn a_queue_whose_writer_stopped_says_it_is_gone() {
        let (link, writer) = link_queue("irp");
        link.queue(b"queued".to_vec()).expect("room");
        let (other, _other_writer) = link_queue("irp");
        assert!(link.same_channel(&link.clone()));
        assert!(!link.same_channel(&other));
        drop(writer);
        assert_eq!(link.queue(b"after".to_vec()), Err(LinkRefusal::Gone));
        assert!(!link.queue_own(b"after".to_vec()));
        tokio::time::timeout(std::time::Duration::from_secs(1), link.closed())
            .await
            .expect("closed");
    }

    /// The writer's lines name the link once its peer is known.
    #[test]
    fn a_links_lines_name_its_kind_and_peer() {
        let (link, writer) = link_queue("node");
        assert_eq!((writer.kind(), writer.peer()), ("node", "-"));
        link.set_peer("AI.sales@motherbee".to_string());
        link.set_peer("ignored".to_string());
        assert_eq!(writer.peer(), "AI.sales@motherbee");
    }
}
