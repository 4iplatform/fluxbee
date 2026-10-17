//! The router's stream (docs/router-stream-v1.md, FINDINGS A-70).
//!
//! A short, best-effort log of what this router copies (turns) and what its nodes append, kept on
//! disk for a small window and delivered in acked batches to its sinks over the fluxbee socket. It
//! is the log plane: the router only hands a record to a bounded queue and never waits (D6); the
//! disk work runs on a thread of its own (D13); delivery runs on
//! tasks of a node of this router, `RT.<router>.stream@<hive>` (D3), so it goes through the normal
//! routing paths and the gateway's LSA announces it.

use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use fluxbee_sdk::node_client::{NodeConfig, NodeUuidMode};
use fluxbee_sdk::protocol::{Destination, Message, Meta, Routing, PRIMARY_HIVE_ID, SYSTEM_KIND};
use fluxbee_sdk::rpc::{
    OperationalRouteProfile, RouteMatch, RouteTarget, RouterDispatcher, SystemRpcRequest,
};
use fluxbee_sdk::stream::{
    stream_node_name, StreamAck, StreamBatch, StreamRecord, MSG_STREAM_ACK, MSG_STREAM_BATCH,
    MSG_STREAM_STATUS, MSG_STREAM_STATUS_RESPONSE, SUBJECT_COGNITION_TURNS,
    SUBJECT_STORAGE_COGNITION_PREFIX, SUBJECT_STORAGE_TURNS, SUBJECT_STORAGE_USAGE,
};
use serde_json::{json, Value};
use tokio::sync::Notify;

// ---- The window and its knobs (D5: visible, easy to change) ----

/// How long a record may wait for its sink. Older records are skipped and counted (D5).
pub const STREAM_MAX_AGE_SECS: u64 = 120;
/// The most a router's stream keeps on disk. Past it, new records are dropped and counted (D5).
pub const STREAM_MAX_BYTES: u64 = 32 * 1024 * 1024;
/// The share of STREAM_MAX_BYTES the nodes' appends may use; the rest is for the turn copies.
pub const STREAM_NODE_APPENDS_SHARE_PERCENT: u64 = 25;
pub const STREAM_SEGMENT_BYTES: u64 = 4 * 1024 * 1024;
pub const STREAM_BATCH_MAX_RECORDS: usize = 256;
pub const STREAM_BATCH_MAX_BYTES: usize = 96 * 1024;
pub const STREAM_ACK_TIMEOUT_SECS: u64 = 5;
pub const STREAM_RETRY_MIN_MS: u64 = 500;
pub const STREAM_RETRY_MAX_MS: u64 = 8_000;
pub const STREAM_FSYNC_INTERVAL_MS: u64 = 250;
pub const STREAM_TURN_QUEUE: usize = 4_096;
pub const STREAM_NODE_APPEND_QUEUE: usize = 1_024;
/// The most payload bytes waiting for the disk thread, both queues together: past it a record is
/// dropped like on a full queue, so a stalled disk cannot grow the router's memory.
pub const STREAM_QUEUE_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// At most one alarm line per interval, with the counts since the last one (D5, "log reducido").
pub const STREAM_ALARM_INTERVAL_SECS: u64 = 30;
/// How often the disk thread trims what every sink took and saves the cursors.
const STREAM_HOUSEKEEPING_INTERVAL_MS: u64 = 1_000;
/// A batch's envelope (stream name, field names) on top of its records.
const BATCH_ENVELOPE_BYTES: usize = 1_024;
/// Per record in a batch: the field names and numbers around subject, producer and payload.
const BATCH_RECORD_OVERHEAD_BYTES: usize = 96;
const STATE_FILE: &str = "state.json";
const SEGMENT_SUFFIX: &str = ".seg";
const RECORD_HEADER_BYTES: usize = 12;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Where a record came from. Turns and node appends share the disk, not the queue nor the cap, so
/// a node appending in a loop cannot crowd out the turns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Turns,
    Nodes,
}

impl Source {
    fn byte(self) -> u8 {
        match self {
            Source::Turns => 0,
            Source::Nodes => 1,
        }
    }
    fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Source::Turns),
            1 => Some(Source::Nodes),
            _ => None,
        }
    }
}

/// Which producer may append which subject (§2.2). Turns, and their copies for cognition, only
/// come from the router itself.
pub fn subject_allowed(subject: &str, producer: &str, hive_id: &str) -> bool {
    if subject == SUBJECT_STORAGE_TURNS || subject == SUBJECT_COGNITION_TURNS {
        return false;
    }
    if subject.starts_with(SUBJECT_STORAGE_COGNITION_PREFIX) {
        return producer.eq_ignore_ascii_case(&format!("SY.cognition@{hive_id}"));
    }
    subject == SUBJECT_STORAGE_USAGE
}

/// Whether SY.cognition runs on a hive of this role: next to the agents (§6.1, D16).
fn role_runs_cognition(role: &str) -> bool {
    matches!(
        role.trim().to_ascii_lowercase().as_str(),
        "motherbee" | "worker"
    )
}

/// The sinks of a subject on a hive of this role (§2.3, §6.2): SY.storage takes the log and
/// everything derived, from every hive; the hive's own SY.cognition takes the copies its router made
/// of its agents' communications.
pub fn sinks_for(subject: &str, role: &str, hive_id: &str) -> Vec<String> {
    if subject == SUBJECT_COGNITION_TURNS {
        return if role_runs_cognition(role) {
            vec![format!("SY.cognition@{hive_id}")]
        } else {
            Vec::new()
        };
    }
    if subject == SUBJECT_STORAGE_TURNS
        || subject == SUBJECT_STORAGE_USAGE
        || subject.starts_with(SUBJECT_STORAGE_COGNITION_PREFIX)
    {
        return vec![format!("SY.storage@{PRIMARY_HIVE_ID}")];
    }
    Vec::new()
}

/// Every sink this stream may deliver to.
fn all_sinks(role: &str, hive_id: &str) -> Vec<String> {
    let mut sinks = Vec::new();
    for subject in [
        SUBJECT_STORAGE_TURNS,
        SUBJECT_STORAGE_USAGE,
        "storage.cognition.threads",
        SUBJECT_COGNITION_TURNS,
    ] {
        for sink in sinks_for(subject, role, hive_id) {
            if !sinks.contains(&sink) {
                sinks.push(sink);
            }
        }
    }
    sinks
}

// ---- Records, on disk and in memory ----

#[derive(Clone, Debug, PartialEq)]
struct Record {
    seq: u64,
    ts_ms: u64,
    source: Source,
    subject: Arc<str>,
    producer: Arc<str>,
    payload: Arc<[u8]>,
}

impl Record {
    fn size(&self) -> u64 {
        (RECORD_HEADER_BYTES + 8 + 8 + 1 + 2 + self.subject.len() + 2 + self.producer.len()) as u64
            + self.payload.len() as u64
    }
}

fn checksum(bytes: &[u8]) -> u32 {
    // FNV-1a, folded to 32 bits: enough to find a torn write at the end of a segment.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    (hash ^ (hash >> 32)) as u32
}

/// `[u32 body len][u32 checksum][body]`, body = seq, ts, source, subject, producer, payload.
fn encode_record(record: &Record) -> Vec<u8> {
    let mut body = Vec::with_capacity(record.size() as usize);
    body.extend_from_slice(&record.seq.to_be_bytes());
    body.extend_from_slice(&record.ts_ms.to_be_bytes());
    body.push(record.source.byte());
    body.extend_from_slice(&(record.subject.len() as u16).to_be_bytes());
    body.extend_from_slice(record.subject.as_bytes());
    body.extend_from_slice(&(record.producer.len() as u16).to_be_bytes());
    body.extend_from_slice(record.producer.as_bytes());
    body.extend_from_slice(&record.payload);
    let mut out = Vec::with_capacity(RECORD_HEADER_BYTES + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&checksum(&body).to_be_bytes());
    out.extend_from_slice(&body);
    out
}

/// The records of a segment, and how many leading bytes are whole. Decoding stops at the first
/// record that is cut short or fails its checksum: a torn write at the end of the newest segment.
fn decode_records(bytes: &[u8]) -> (Vec<Record>, usize) {
    let mut records = Vec::new();
    let mut at = 0usize;
    while bytes.len() - at >= RECORD_HEADER_BYTES {
        let len = u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap()) as usize;
        let sum = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap());
        let start = at + 8;
        let Some(body) = bytes.get(start..start + len) else {
            break;
        };
        if checksum(body) != sum {
            break;
        }
        let Some(record) = decode_body(body) else {
            break;
        };
        records.push(record);
        at = start + len;
    }
    (records, at)
}

fn decode_body(body: &[u8]) -> Option<Record> {
    let mut at = 0usize;
    let mut take = |n: usize| -> Option<&[u8]> {
        let slice = body.get(at..at + n)?;
        at += n;
        Some(slice)
    };
    let seq = u64::from_be_bytes(take(8)?.try_into().ok()?);
    let ts_ms = u64::from_be_bytes(take(8)?.try_into().ok()?);
    let source = Source::from_byte(take(1)?[0])?;
    let subject_len = u16::from_be_bytes(take(2)?.try_into().ok()?) as usize;
    let subject = std::str::from_utf8(take(subject_len)?).ok()?.to_string();
    let producer_len = u16::from_be_bytes(take(2)?.try_into().ok()?) as usize;
    let producer = std::str::from_utf8(take(producer_len)?).ok()?.to_string();
    let payload = body.get(at..)?.to_vec();
    Some(Record {
        seq,
        ts_ms,
        source,
        subject: Arc::from(subject),
        producer: Arc::from(producer),
        payload: Arc::from(payload),
    })
}

// ---- What the router holds: a queue it never waits on ----

/// What a test reads back from a `StreamHandle::observed()`.
#[cfg(test)]
pub struct ObservedStream {
    rx: std_mpsc::Receiver<Incoming>,
    counters: Arc<Counters>,
}

#[cfg(test)]
impl ObservedStream {
    /// The appends so far, as (subject, producer, payload).
    pub fn drain(&self) -> Vec<(String, String, Vec<u8>)> {
        self.rx
            .try_iter()
            .map(|incoming| (incoming.subject, incoming.producer, incoming.payload))
            .collect()
    }

    pub fn refused(&self) -> u64 {
        self.counters.refused.load(Ordering::Relaxed)
    }
}

struct Incoming {
    source: Source,
    ts_ms: u64,
    subject: String,
    producer: String,
    payload: Vec<u8>,
}

/// The counters the hot path and the disk thread bump without a lock.
#[derive(Default)]
struct Counters {
    appended_turns: AtomicU64,
    appended_nodes: AtomicU64,
    dropped_queue_turns: AtomicU64,
    dropped_queue_nodes: AtomicU64,
    dropped_cap_turns: AtomicU64,
    dropped_cap_nodes: AtomicU64,
    refused: AtomicU64,
    disk_errors: AtomicU64,
}

/// The router's side of the stream: append and forget (D6).
#[derive(Clone)]
pub struct StreamHandle {
    tx: std_mpsc::SyncSender<Incoming>,
    turns_in_queue: Arc<AtomicUsize>,
    nodes_in_queue: Arc<AtomicUsize>,
    queued_bytes: Arc<AtomicU64>,
    counters: Arc<Counters>,
}

impl StreamHandle {
    /// A handle whose stream is gone: every append is dropped and counted. For tests that drive a
    /// node connection without a stream.
    #[cfg(test)]
    pub fn detached() -> Self {
        let (tx, _rx) = std_mpsc::sync_channel(1);
        Self {
            tx,
            turns_in_queue: Arc::new(AtomicUsize::new(0)),
            nodes_in_queue: Arc::new(AtomicUsize::new(0)),
            queued_bytes: Arc::new(AtomicU64::new(0)),
            counters: Arc::new(Counters::default()),
        }
    }

    /// A handle whose appends a test reads back: (subject, producer, payload) in order, and the
    /// refused count.
    #[cfg(test)]
    pub fn observed() -> (Self, ObservedStream) {
        let (tx, rx) = std_mpsc::sync_channel(64);
        let counters = Arc::new(Counters::default());
        (
            Self {
                tx,
                turns_in_queue: Arc::new(AtomicUsize::new(0)),
                nodes_in_queue: Arc::new(AtomicUsize::new(0)),
                queued_bytes: Arc::new(AtomicU64::new(0)),
                counters: Arc::clone(&counters),
            },
            ObservedStream { rx, counters },
        )
    }

    /// The router's copy of a turn: the message as it routed it, with the sender's real name.
    pub fn append_turn(&self, producer: &str, payload: Vec<u8>) {
        self.append(Source::Turns, SUBJECT_STORAGE_TURNS, producer, payload);
    }

    /// The router's copy, for its hive's SY.cognition, of a message a local agent sent or received;
    /// the producer is that agent's node (§6.2).
    pub fn append_cognition_turn(&self, agent_node: &str, payload: Vec<u8>) {
        self.append(Source::Turns, SUBJECT_COGNITION_TURNS, agent_node, payload);
    }

    /// A node's `STREAM_APPEND`, already checked with `subject_allowed`.
    pub fn append_from_node(&self, subject: &str, producer: &str, payload: Vec<u8>) {
        self.append(Source::Nodes, subject, producer, payload);
    }

    /// A node's `STREAM_APPEND` that `subject_allowed` refused.
    pub fn count_refused(&self) {
        self.counters.refused.fetch_add(1, Ordering::Relaxed);
    }

    fn append(&self, source: Source, subject: &str, producer: &str, payload: Vec<u8>) {
        let (in_queue, limit, dropped) = match source {
            Source::Turns => (
                &self.turns_in_queue,
                STREAM_TURN_QUEUE,
                &self.counters.dropped_queue_turns,
            ),
            Source::Nodes => (
                &self.nodes_in_queue,
                STREAM_NODE_APPEND_QUEUE,
                &self.counters.dropped_queue_nodes,
            ),
        };
        let bytes = payload.len() as u64;
        if in_queue.fetch_add(1, Ordering::Relaxed) >= limit {
            in_queue.fetch_sub(1, Ordering::Relaxed);
            dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if self.queued_bytes.fetch_add(bytes, Ordering::Relaxed) + bytes > STREAM_QUEUE_MAX_BYTES {
            self.queued_bytes.fetch_sub(bytes, Ordering::Relaxed);
            in_queue.fetch_sub(1, Ordering::Relaxed);
            dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let incoming = Incoming {
            source,
            ts_ms: now_ms(),
            subject: subject.to_string(),
            producer: producer.to_string(),
            payload,
        };
        if self.tx.try_send(incoming).is_err() {
            self.queued_bytes.fetch_sub(bytes, Ordering::Relaxed);
            in_queue.fetch_sub(1, Ordering::Relaxed);
            dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

// ---- The log, shared by the disk thread and the senders ----

#[derive(Clone, Debug, Default)]
struct SinkState {
    cursor: u64,
    /// The last seq of the batch waiting for its ack, 0 when none: its records stay in the log
    /// until the ack (or its failure) says whether the sink took them.
    in_flight_up_to: u64,
    delivered: u64,
    /// Records this sink lost: past the window, or too big for a frame. Each is counted once,
    /// when the cursor passes it or when it leaves the log before the sink took it (D10).
    expired: u64,
    oversize: u64,
    failures: u64,
    last_ack_ms: u64,
}

struct Shared {
    records: VecDeque<Record>,
    turn_bytes: u64,
    node_bytes: u64,
    next_seq: u64,
    fsynced_seq: u64,
    sinks: HashMap<String, SinkState>,
    role: String,
    hive_id: String,
}

impl Shared {
    fn sinks_of(&self, subject: &str) -> Vec<String> {
        sinks_for(subject, &self.role, &self.hive_id)
    }

    /// Whether the record may leave the log: every sink took it, or it is past the window and no
    /// batch carrying it waits for an ack (the ack says whether that sink took it).
    fn record_done(&self, record: &Record, now: u64) -> bool {
        let aged = now.saturating_sub(record.ts_ms) > STREAM_MAX_AGE_SECS * 1000;
        self.sinks_of(&record.subject)
            .iter()
            .all(|sink| match self.sinks.get(sink) {
                Some(state) if state.cursor >= record.seq => true,
                Some(state) => aged && state.in_flight_up_to < record.seq,
                None => aged,
            })
    }

    /// Drops the leading records that are done; returns the first seq still kept. A record that
    /// leaves before one of its sinks took it is counted as expired for that sink, here and only
    /// here: a sink's cursor never passes it afterwards without it.
    fn trim(&mut self, now: u64) -> u64 {
        while let Some(front) = self.records.front() {
            if !self.record_done(front, now) {
                break;
            }
            let record = self.records.pop_front().expect("front exists");
            for sink in self.sinks_of(&record.subject) {
                if let Some(state) = self.sinks.get_mut(&sink) {
                    if state.cursor < record.seq {
                        state.expired += 1;
                    }
                }
            }
            match record.source {
                Source::Turns => self.turn_bytes = self.turn_bytes.saturating_sub(record.size()),
                Source::Nodes => self.node_bytes = self.node_bytes.saturating_sub(record.size()),
            }
        }
        self.records
            .front()
            .map(|record| record.seq)
            .unwrap_or(self.next_seq)
    }

    /// Whether a record of this size fits in its source's share of the cap.
    fn fits(&self, source: Source, size: u64) -> bool {
        let node_cap = STREAM_MAX_BYTES * STREAM_NODE_APPENDS_SHARE_PERCENT / 100;
        let total = self.turn_bytes + self.node_bytes + size;
        match source {
            Source::Turns => {
                total <= STREAM_MAX_BYTES && self.turn_bytes + size <= STREAM_MAX_BYTES - node_cap
            }
            Source::Nodes => total <= STREAM_MAX_BYTES && self.node_bytes + size <= node_cap,
        }
    }
}

/// What one sender does next.
#[derive(Debug, PartialEq)]
enum BatchPlan {
    /// Nothing for this sink.
    Idle,
    /// Only records this sink does not take (or skips): its cursor moved, nothing to send.
    Skipped,
    Send {
        records: Vec<Record>,
        last_seq: u64,
        scanned_up_to: u64,
    },
}

/// The next batch for `sink`, from its cursor (§2.5). Records of other sinks are passed over.
/// Records this sink loses (past the window, too big for a frame) are passed and counted only
/// ahead of the batch, where the cursor moves past them right away; one after the first record of
/// the batch ends it, and the next plan passes it. So each is counted once, however often the
/// batch is sent again.
fn plan_batch(shared: &mut Shared, sink: &str, now: u64) -> BatchPlan {
    let cursor = shared.sinks.get(sink).map(|s| s.cursor).unwrap_or(0);
    // Up to `passed`: resolved ahead of the batch. Up to `scanned`: what a full ack resolves.
    let mut passed = cursor;
    let mut scanned = cursor;
    let mut out: Vec<Record> = Vec::new();
    let mut bytes = BATCH_ENVELOPE_BYTES;
    let mut expired = 0u64;
    let mut oversize = 0u64;
    for record in shared.records.iter() {
        if record.seq <= cursor {
            continue;
        }
        if !sinks_for(&record.subject, &shared.role, &shared.hive_id)
            .iter()
            .any(|s| s == sink)
        {
            scanned = record.seq;
            if out.is_empty() {
                passed = record.seq;
            }
            continue;
        }
        let size = record.payload.len()
            + record.subject.len()
            + record.producer.len()
            + BATCH_RECORD_OVERHEAD_BYTES;
        let aged = now.saturating_sub(record.ts_ms) > STREAM_MAX_AGE_SECS * 1000;
        let too_big = size + BATCH_ENVELOPE_BYTES > STREAM_BATCH_MAX_BYTES;
        if aged || too_big {
            if !out.is_empty() {
                break;
            }
            if aged {
                expired += 1;
            } else {
                oversize += 1;
            }
            passed = record.seq;
            scanned = record.seq;
            continue;
        }
        if !out.is_empty()
            && (out.len() >= STREAM_BATCH_MAX_RECORDS || bytes + size > STREAM_BATCH_MAX_BYTES)
        {
            break;
        }
        bytes += size;
        scanned = record.seq;
        out.push(record.clone());
    }
    let state = shared.sinks.entry(sink.to_string()).or_default();
    state.expired += expired;
    state.oversize += oversize;
    if passed > state.cursor {
        state.cursor = passed;
    }
    let Some(last_seq) = out.last().map(|r| r.seq) else {
        return if passed > cursor {
            BatchPlan::Skipped
        } else {
            BatchPlan::Idle
        };
    };
    state.in_flight_up_to = last_seq;
    BatchPlan::Send {
        records: out,
        last_seq,
        scanned_up_to: scanned,
    }
}

/// The ack's new cursor, when it is the sink's and covers the batch in flight (§2.5). STREAM_*
/// are not protected actions, so anyone could send one; an ack from anyone else, or past what was
/// sent, moves nothing.
fn accept_ack(
    msg: &Message,
    sink: &str,
    cursor: u64,
    last_seq: u64,
    scanned_up_to: u64,
) -> Option<u64> {
    if msg.meta.msg.as_deref() != Some(MSG_STREAM_ACK) {
        return None;
    }
    let from = msg.routing.src_l2_name.as_deref()?;
    if !from.eq_ignore_ascii_case(sink) {
        return None;
    }
    let ack: StreamAck = serde_json::from_value(msg.payload.clone()).ok()?;
    if ack.up_to_seq <= cursor || ack.up_to_seq > last_seq {
        return None;
    }
    if ack.up_to_seq == last_seq {
        Some(scanned_up_to)
    } else {
        Some(ack.up_to_seq)
    }
}

fn batch_payload(stream: &str, epoch: u64, records: &[Record]) -> Value {
    let records: Vec<StreamRecord> = records
        .iter()
        .map(|record| StreamRecord {
            seq: record.seq,
            ts_ms: record.ts_ms,
            subject: record.subject.to_string(),
            producer: record.producer.to_string(),
            payload: serde_json::from_slice(&record.payload).unwrap_or_else(|_| {
                Value::String(String::from_utf8_lossy(&record.payload).into_owned())
            }),
        })
        .collect();
    serde_json::to_value(StreamBatch {
        stream: stream.to_string(),
        epoch,
        records,
    })
    .unwrap_or(Value::Null)
}

/// A new epoch: a sink keys its last seq by stream and epoch, so a stream whose seq restarts is
/// never mistaken for a repeat (§6.2).
fn new_epoch() -> u64 {
    uuid::Uuid::new_v4().as_u64_pair().0.max(1)
}

// ---- The disk ----

struct Segment {
    last_seq: u64,
    path: PathBuf,
}

struct Disk {
    dir: PathBuf,
    segments: Vec<Segment>,
    current: Option<(File, u64)>,
    last_written_seq: u64,
    dirty: bool,
}

fn segment_path(dir: &Path, first_seq: u64) -> PathBuf {
    dir.join(format!("{first_seq:020}{SEGMENT_SUFFIX}"))
}

impl Disk {
    fn append(&mut self, record: &Record) -> io::Result<()> {
        let written = self.write(record);
        if written.is_err() {
            // A partial write would cut this segment short for every record after it (recovery
            // stops at the tear): the next record starts a new segment.
            self.current = None;
        }
        written
    }

    fn write(&mut self, record: &Record) -> io::Result<()> {
        let roll = match &self.current {
            Some((_, size)) => *size >= STREAM_SEGMENT_BYTES,
            None => true,
        };
        if roll {
            if let Some((file, _)) = self.current.take() {
                file.sync_data()?;
            }
            let path = segment_path(&self.dir, record.seq);
            let file = OpenOptions::new().create(true).append(true).open(&path)?;
            self.segments.push(Segment {
                last_seq: record.seq,
                path,
            });
            self.current = Some((file, 0));
        }
        let bytes = encode_record(record);
        let (file, size) = self.current.as_mut().expect("a current segment");
        file.write_all(&bytes)?;
        *size += bytes.len() as u64;
        if let Some(segment) = self.segments.last_mut() {
            segment.last_seq = record.seq;
        }
        self.last_written_seq = record.seq;
        self.dirty = true;
        Ok(())
    }

    fn fsync(&mut self) -> io::Result<Option<u64>> {
        if !self.dirty {
            return Ok(None);
        }
        if let Some((file, _)) = &self.current {
            file.sync_data()?;
        }
        self.dirty = false;
        Ok(Some(self.last_written_seq))
    }

    /// Deletes the segments whose records are all below `keep_from`. With `nothing_left` (every
    /// record taken or passed, and the state saved) the newest goes too: a record stays on disk
    /// only until its sinks took it (§4), and `state.json` keeps the next seq.
    fn trim(&mut self, keep_from: u64, nothing_left: bool) {
        if nothing_left {
            self.current = None;
            self.dirty = false;
            for segment in self.segments.drain(..) {
                let _ = fs::remove_file(&segment.path);
            }
            return;
        }
        while self.segments.len() > 1 {
            let first = &self.segments[0];
            if first.last_seq >= keep_from {
                break;
            }
            let _ = fs::remove_file(&first.path);
            self.segments.remove(0);
        }
    }

    fn save_state(
        &self,
        epoch: u64,
        next_seq: u64,
        cursors: &HashMap<String, u64>,
    ) -> io::Result<()> {
        let body =
            serde_json::to_vec(&json!({"epoch": epoch, "next_seq": next_seq, "cursors": cursors}))
                .map_err(io::Error::other)?;
        let tmp = self.dir.join(format!("{STATE_FILE}.tmp"));
        fs::write(&tmp, body)?;
        fs::rename(tmp, self.dir.join(STATE_FILE))
    }
}

/// What a restart finds: the records still inside the window, the next seq and the cursors.
struct Recovered {
    disk: Option<Disk>,
    records: Vec<Record>,
    next_seq: u64,
    cursors: HashMap<String, u64>,
    /// The saved epoch, or a new one when `state.json` is missing or unreadable.
    epoch: u64,
}

/// What is on disk. The records past the window are dropped (and counted) by the first trim.
fn recover(dir: &Path, sinks: &[String]) -> io::Result<Recovered> {
    fs::create_dir_all(dir)?;
    let mut paths: Vec<(u64, PathBuf)> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let first = name.strip_suffix(SEGMENT_SUFFIX)?.parse::<u64>().ok()?;
            Some((first, path))
        })
        .collect();
    paths.sort_by_key(|(first, _)| *first);
    let mut records = Vec::new();
    let mut segments = Vec::new();
    let count = paths.len();
    for (index, (first_seq, path)) in paths.into_iter().enumerate() {
        let mut bytes = Vec::new();
        File::open(&path)?.read_to_end(&mut bytes)?;
        let (decoded, whole) = decode_records(&bytes);
        if whole < bytes.len() && index + 1 == count {
            // A torn write at the end of the newest segment: cut it there.
            OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(whole as u64)?;
        }
        let last_seq = decoded.last().map(|r| r.seq).unwrap_or(first_seq);
        records.extend(decoded);
        segments.push(Segment { last_seq, path });
    }
    let last_on_disk = records.last().map(|r| r.seq).unwrap_or(0);
    let state: Value = fs::read(dir.join(STATE_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null);
    let saved_next = state.get("next_seq").and_then(Value::as_u64).unwrap_or(1);
    let epoch = state
        .get("epoch")
        .and_then(Value::as_u64)
        .filter(|epoch| *epoch > 0)
        .unwrap_or_else(new_epoch);
    let next_seq = saved_next.max(last_on_disk + 1).max(1);
    let mut cursors = HashMap::new();
    if let Some(saved) = state.get("cursors").and_then(Value::as_object) {
        for sink in sinks {
            if let Some(cursor) = saved.get(sink).and_then(Value::as_u64) {
                // Never past what is on disk: a cursor saved for records a power cut lost would
                // otherwise count new records as taken.
                cursors.insert(sink.clone(), cursor.min(last_on_disk));
            }
        }
    }
    let current = match segments.last() {
        Some(segment) => {
            let file = OpenOptions::new().append(true).open(&segment.path)?;
            let size = file.metadata()?.len();
            Some((file, size))
        }
        None => None,
    };
    Ok(Recovered {
        disk: Some(Disk {
            dir: dir.to_path_buf(),
            segments,
            current,
            last_written_seq: last_on_disk,
            dirty: false,
        }),
        records,
        next_seq,
        cursors,
        epoch,
    })
}

// ---- Opening the stream ----

/// What runs the stream once the router accepts nodes.
pub struct StreamRuntime {
    shared: Arc<StdMutex<Shared>>,
    notify: Arc<Notify>,
    counters: Arc<Counters>,
    sinks: Vec<String>,
    node: NodeConfig,
    epoch: u64,
}

/// Opens the stream of this router: recovers its log and starts its disk thread. The returned
/// handle is what the router appends to; the runtime starts the node once the router listens.
pub fn open(
    router_l2_name: &str,
    hive_id: &str,
    role: &str,
    state_dir: &Path,
    config_dir: &Path,
    node_socket_path: &Path,
) -> (StreamHandle, StreamRuntime) {
    let sinks = all_sinks(role, hive_id);
    let dir = state_dir.join(router_l2_name).join("stream");
    let now = now_ms();
    let recovered = recover(&dir, &sinks).unwrap_or_else(|err| {
        // The router runs anyway: the stream keeps its records in memory only.
        tracing::error!(dir = %dir.display(), error = %err, "router stream cannot use its directory; keeping records in memory only");
        Recovered {
            disk: None,
            records: Vec::new(),
            next_seq: 1,
            cursors: HashMap::new(),
            epoch: new_epoch(),
        }
    });
    let epoch = recovered.epoch;
    let mut shared = Shared {
        records: VecDeque::new(),
        turn_bytes: 0,
        node_bytes: 0,
        next_seq: recovered.next_seq,
        fsynced_seq: recovered.next_seq.saturating_sub(1),
        sinks: HashMap::new(),
        role: role.to_string(),
        hive_id: hive_id.to_string(),
    };
    for sink in &sinks {
        let cursor = recovered.cursors.get(sink).copied().unwrap_or(0);
        shared.sinks.insert(
            sink.clone(),
            SinkState {
                cursor,
                ..SinkState::default()
            },
        );
    }
    for record in recovered.records {
        match record.source {
            Source::Turns => shared.turn_bytes += record.size(),
            Source::Nodes => shared.node_bytes += record.size(),
        }
        shared.records.push_back(record);
    }
    // What aged out while the router was down leaves now, counted as expired for its sinks.
    shared.trim(now);
    let shared = Arc::new(StdMutex::new(shared));
    let notify = Arc::new(Notify::new());
    let counters = Arc::new(Counters::default());
    let (tx, rx) = std_mpsc::sync_channel(STREAM_TURN_QUEUE + STREAM_NODE_APPEND_QUEUE);
    let handle = StreamHandle {
        tx,
        turns_in_queue: Arc::new(AtomicUsize::new(0)),
        nodes_in_queue: Arc::new(AtomicUsize::new(0)),
        queued_bytes: Arc::new(AtomicU64::new(0)),
        counters: Arc::clone(&counters),
    };
    let thread_shared = Arc::clone(&shared);
    let thread_notify = Arc::clone(&notify);
    let thread_counters = Arc::clone(&counters);
    let queues = QueueCounts {
        turns: Arc::clone(&handle.turns_in_queue),
        nodes: Arc::clone(&handle.nodes_in_queue),
        bytes: Arc::clone(&handle.queued_bytes),
    };
    let disk = recovered.disk;
    let stream_name = stream_node_name(router_l2_name);
    let spawned = std::thread::Builder::new()
        .name("router-stream-disk".to_string())
        .spawn(move || {
            run_disk(
                rx,
                disk,
                thread_shared,
                thread_notify,
                thread_counters,
                queues,
                stream_name,
                epoch,
            )
        });
    if let Err(err) = spawned {
        tracing::error!(error = %err, "router stream cannot start its disk thread; records will be dropped");
    }
    let base = stream_node_name(router_l2_name)
        .split('@')
        .next()
        .unwrap_or_default()
        .to_string();
    let node = NodeConfig {
        name: base,
        router_socket: node_socket_path.to_path_buf(),
        uuid_persistence_dir: state_dir.join(router_l2_name),
        uuid_mode: NodeUuidMode::Persistent,
        config_dir: config_dir.to_path_buf(),
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    (
        handle,
        StreamRuntime {
            shared,
            notify,
            counters,
            sinks,
            node,
            epoch,
        },
    )
}

/// What waits in the queue to the disk thread, which gives it back as it takes each record.
struct QueueCounts {
    turns: Arc<AtomicUsize>,
    nodes: Arc<AtomicUsize>,
    bytes: Arc<AtomicU64>,
}

fn run_disk(
    rx: std_mpsc::Receiver<Incoming>,
    mut disk: Option<Disk>,
    shared: Arc<StdMutex<Shared>>,
    notify: Arc<Notify>,
    counters: Arc<Counters>,
    queues: QueueCounts,
    stream_name: String,
    epoch: u64,
) {
    let fsync_every = Duration::from_millis(STREAM_FSYNC_INTERVAL_MS);
    let housekeeping_every = Duration::from_millis(STREAM_HOUSEKEEPING_INTERVAL_MS);
    let alarm_every = Duration::from_secs(STREAM_ALARM_INTERVAL_SECS);
    let mut last_fsync = Instant::now();
    let mut last_housekeeping = Instant::now();
    let mut last_alarm = Instant::now();
    let mut alarm_seen = AlarmSnapshot::default();
    let mut last_saved: Option<(u64, HashMap<String, u64>)> = None;
    loop {
        match rx.recv_timeout(fsync_every) {
            Ok(incoming) => {
                match incoming.source {
                    Source::Turns => queues.turns.fetch_sub(1, Ordering::Relaxed),
                    Source::Nodes => queues.nodes.fetch_sub(1, Ordering::Relaxed),
                };
                queues
                    .bytes
                    .fetch_sub(incoming.payload.len() as u64, Ordering::Relaxed);
                take_incoming(incoming, &mut disk, &shared, &counters);
                notify.notify_waiters();
            }
            Err(std_mpsc::RecvTimeoutError::Timeout) => {}
            Err(std_mpsc::RecvTimeoutError::Disconnected) => return,
        }
        if last_fsync.elapsed() >= fsync_every {
            last_fsync = Instant::now();
            if let Some(disk) = disk.as_mut() {
                match disk.fsync() {
                    Ok(Some(seq)) => {
                        if let Ok(mut shared) = shared.lock() {
                            shared.fsynced_seq = seq;
                        }
                    }
                    Ok(None) => {}
                    Err(_) => {
                        counters.disk_errors.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
        }
        if last_housekeeping.elapsed() >= housekeeping_every {
            last_housekeeping = Instant::now();
            let (keep_from, nothing_left, next_seq, cursors) = {
                let Ok(mut shared) = shared.lock() else {
                    continue;
                };
                let keep_from = shared.trim(now_ms());
                let fsynced = shared.fsynced_seq;
                let cursors: HashMap<String, u64> = shared
                    .sinks
                    .iter()
                    .map(|(sink, state)| (sink.clone(), state.cursor.min(fsynced)))
                    .collect();
                (
                    keep_from,
                    shared.records.is_empty(),
                    shared.next_seq,
                    cursors,
                )
            };
            if let Some(disk) = disk.as_mut() {
                // The state first: the newest segment goes only once the next seq is saved.
                let state_now = (next_seq, cursors);
                let saved = if last_saved.as_ref() == Some(&state_now) {
                    true
                } else if disk.save_state(epoch, state_now.0, &state_now.1).is_ok() {
                    last_saved = Some(state_now);
                    true
                } else {
                    counters.disk_errors.fetch_add(1, Ordering::Relaxed);
                    false
                };
                disk.trim(keep_from, nothing_left && saved);
            }
            // Wakes the senders now and then, so a record past the window is counted and passed
            // even when nothing new arrives.
            notify.notify_waiters();
        }
        if last_alarm.elapsed() >= alarm_every {
            last_alarm = Instant::now();
            let now = AlarmSnapshot::take(&counters, &shared);
            if let Some(line) = now.since(&alarm_seen) {
                tracing::warn!(stream = %stream_name, "{line}");
            }
            alarm_seen = now;
        }
    }
}

/// Places a record in the log under the lock and writes it to disk outside it: the senders and
/// the status take that lock on the router's runtime threads, which never wait on the disk (D13).
/// Only this thread writes, so the disk keeps the seq order.
fn take_incoming(
    incoming: Incoming,
    disk: &mut Option<Disk>,
    shared: &Arc<StdMutex<Shared>>,
    counters: &Counters,
) {
    let record = {
        let Ok(mut state) = shared.lock() else {
            return;
        };
        let record = Record {
            seq: state.next_seq,
            ts_ms: incoming.ts_ms,
            source: incoming.source,
            subject: Arc::from(incoming.subject),
            producer: Arc::from(incoming.producer),
            payload: Arc::from(incoming.payload),
        };
        let size = record.size();
        if !state.fits(record.source, size) {
            // Make room first from what every sink already took or what aged out; never from
            // records still waiting (D5: past the cap, new records are the ones dropped).
            state.trim(now_ms());
            if !state.fits(record.source, size) {
                match record.source {
                    Source::Turns => counters.dropped_cap_turns.fetch_add(1, Ordering::Relaxed),
                    Source::Nodes => counters.dropped_cap_nodes.fetch_add(1, Ordering::Relaxed),
                };
                return;
            }
        }
        state.next_seq += 1;
        match record.source {
            Source::Turns => {
                state.turn_bytes += size;
                counters.appended_turns.fetch_add(1, Ordering::Relaxed);
            }
            Source::Nodes => {
                state.node_bytes += size;
                counters.appended_nodes.fetch_add(1, Ordering::Relaxed);
            }
        }
        state.records.push_back(record.clone());
        record
    };
    if let Some(disk) = disk.as_mut() {
        if disk.append(&record).is_err() {
            counters.disk_errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The drop counters at one moment, to report what changed since the last alarm line.
#[derive(Default, Clone, PartialEq)]
struct AlarmSnapshot {
    dropped_queue: u64,
    dropped_cap: u64,
    refused: u64,
    disk_errors: u64,
    expired: u64,
    oversize: u64,
}

impl AlarmSnapshot {
    fn take(counters: &Counters, shared: &Arc<StdMutex<Shared>>) -> Self {
        let (expired, oversize) = shared
            .lock()
            .map(|shared| {
                shared.sinks.values().fold((0, 0), |(e, o), state| {
                    (e + state.expired, o + state.oversize)
                })
            })
            .unwrap_or((0, 0));
        Self {
            dropped_queue: counters.dropped_queue_turns.load(Ordering::Relaxed)
                + counters.dropped_queue_nodes.load(Ordering::Relaxed),
            dropped_cap: counters.dropped_cap_turns.load(Ordering::Relaxed)
                + counters.dropped_cap_nodes.load(Ordering::Relaxed),
            refused: counters.refused.load(Ordering::Relaxed),
            disk_errors: counters.disk_errors.load(Ordering::Relaxed),
            expired,
            oversize,
        }
    }

    /// One line with what grew since `before`, or nothing when nothing did.
    fn since(&self, before: &Self) -> Option<String> {
        let parts: Vec<String> = [
            (
                "dropped (queue full)",
                self.dropped_queue,
                before.dropped_queue,
            ),
            ("dropped (cap)", self.dropped_cap, before.dropped_cap),
            ("refused appends", self.refused, before.refused),
            ("expired", self.expired, before.expired),
            ("oversize", self.oversize, before.oversize),
            ("disk errors", self.disk_errors, before.disk_errors),
        ]
        .iter()
        .filter(|(_, now, then)| now > then)
        .map(|(what, now, then)| format!("{what}: {}", now - then))
        .collect();
        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "router stream lost records in the last {STREAM_ALARM_INTERVAL_SECS} s: {}",
            parts.join(", ")
        ))
    }
}

// ---- The node: delivery and status ----

const RPC_CH_SYSTEM: &str = "system";

/// Connects the stream to its own router as a node and runs its senders and its status. Spawned
/// once the router listens, never awaited by it (§2.1).
pub async fn run_node(runtime: StreamRuntime) {
    let profile = match OperationalRouteProfile::builder()
        .command_channel(RPC_CH_SYSTEM)
        .post_pending_rule(
            RouteMatch::any_msg_type(SYSTEM_KIND),
            RouteTarget::Command(RPC_CH_SYSTEM),
        )
        .build()
    {
        Ok(profile) => profile,
        Err(err) => {
            tracing::error!(error = %err, "router stream rpc profile invalid; the stream will not deliver");
            return;
        }
    };
    let dispatcher = match RouterDispatcher::connect_with_retry(
        runtime.node.clone(),
        Duration::from_secs(1),
        profile,
    )
    .await
    {
        Ok(dispatcher) => dispatcher,
        Err(err) => {
            tracing::error!(error = %err, "router stream could not join its router; the stream will not deliver");
            return;
        }
    };
    let stream_name = dispatcher.sender_snapshot().full_name().to_string();
    tracing::info!(stream = %stream_name, sinks = ?runtime.sinks, "router stream joined its router");
    for sink in &runtime.sinks {
        std::mem::drop(tokio::spawn(run_sink(
            sink.clone(),
            stream_name.clone(),
            runtime.epoch,
            Arc::clone(&runtime.shared),
            Arc::clone(&runtime.notify),
            Arc::clone(&dispatcher),
        )));
    }
    let mut rx = match dispatcher.take_command_receiver(RPC_CH_SYSTEM).await {
        Ok(rx) => rx,
        Err(err) => {
            tracing::error!(error = %err, "router stream has no command channel; it delivers but answers no status");
            return;
        }
    };
    while let Some(msg) = rx.recv().await {
        if msg.meta.msg.as_deref() != Some(MSG_STREAM_STATUS) {
            continue;
        }
        let status = status_payload(&stream_name, &runtime.shared, &runtime.counters);
        let sender = dispatcher.sender_snapshot();
        let reply = Message {
            routing: Routing {
                src: sender.uuid().to_string(),
                src_l2_name: None,
                dst: Destination::Unicast(msg.routing.src.clone()),
                ttl: msg.routing.ttl.max(1),
                trace_id: msg.routing.trace_id.clone(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_STREAM_STATUS_RESPONSE.to_string()),
                ..Meta::default()
            },
            payload: status,
        };
        if let Err(err) = sender.send(reply).await {
            tracing::warn!(error = %err, "router stream could not answer a status request");
        }
    }
}

async fn run_sink(
    sink: String,
    stream_name: String,
    epoch: u64,
    shared: Arc<StdMutex<Shared>>,
    notify: Arc<Notify>,
    dispatcher: Arc<RouterDispatcher>,
) {
    let mut backoff = STREAM_RETRY_MIN_MS;
    loop {
        let plan = match shared.lock() {
            Ok(mut shared) => plan_batch(&mut shared, &sink, now_ms()),
            Err(_) => return,
        };
        let (records, last_seq, scanned_up_to) = match plan {
            BatchPlan::Idle => {
                tokio::select! {
                    _ = notify.notified() => {}
                    _ = tokio::time::sleep(Duration::from_millis(STREAM_HOUSEKEEPING_INTERVAL_MS)) => {}
                }
                continue;
            }
            BatchPlan::Skipped => continue,
            BatchPlan::Send {
                records,
                last_seq,
                scanned_up_to,
            } => (records, last_seq, scanned_up_to),
        };
        let payload = batch_payload(&stream_name, epoch, &records);
        let result = dispatcher
            .send_system_rpc(SystemRpcRequest {
                target: &sink,
                request_msg: MSG_STREAM_BATCH,
                response_msg: MSG_STREAM_ACK,
                payload,
                timeout: Duration::from_secs(STREAM_ACK_TIMEOUT_SECS),
            })
            .await;
        if let Err(err) = &result {
            tracing::debug!(sink = %sink, error = %err, "router stream batch not acked; retrying");
        }
        let accepted = {
            let Ok(mut shared) = shared.lock() else {
                return;
            };
            let state = shared.sinks.entry(sink.clone()).or_default();
            state.in_flight_up_to = 0;
            let cursor = result
                .as_ref()
                .ok()
                .and_then(|reply| accept_ack(reply, &sink, state.cursor, last_seq, scanned_up_to));
            match cursor {
                Some(cursor) => {
                    state.delivered += records.iter().filter(|r| r.seq <= cursor).count() as u64;
                    state.cursor = cursor;
                    state.last_ack_ms = now_ms();
                    true
                }
                None => {
                    state.failures += 1;
                    false
                }
            }
        };
        if accepted {
            backoff = STREAM_RETRY_MIN_MS;
        } else {
            tokio::time::sleep(Duration::from_millis(backoff)).await;
            backoff = (backoff * 2).min(STREAM_RETRY_MAX_MS);
        }
    }
}

fn status_payload(stream_name: &str, shared: &Arc<StdMutex<Shared>>, counters: &Counters) -> Value {
    let now = now_ms();
    let (backlog, sinks, next_seq, fsynced_seq) = match shared.lock() {
        Ok(shared) => {
            let oldest_age_ms = shared
                .records
                .front()
                .map(|record| now.saturating_sub(record.ts_ms))
                .unwrap_or(0);
            let sinks: serde_json::Map<String, Value> = shared
                .sinks
                .iter()
                .map(|(sink, state)| {
                    let pending = shared
                        .records
                        .iter()
                        .filter(|r| r.seq > state.cursor)
                        .filter(|r| shared.sinks_of(&r.subject).contains(sink))
                        .count();
                    (
                        sink.clone(),
                        json!({
                            "cursor": state.cursor,
                            "pending": pending,
                            "delivered": state.delivered,
                            "expired": state.expired,
                            "oversize": state.oversize,
                            "failures": state.failures,
                            "last_ack_ms": state.last_ack_ms,
                        }),
                    )
                })
                .collect();
            (
                json!({
                    "records": shared.records.len(),
                    "bytes": shared.turn_bytes + shared.node_bytes,
                    "turn_bytes": shared.turn_bytes,
                    "node_bytes": shared.node_bytes,
                    "oldest_age_ms": oldest_age_ms,
                }),
                Value::Object(sinks),
                shared.next_seq,
                shared.fsynced_seq,
            )
        }
        Err(_) => (Value::Null, Value::Null, 0, 0),
    };
    json!({
        "status": "ok",
        "stream": stream_name,
        "window": {"max_age_secs": STREAM_MAX_AGE_SECS, "max_bytes": STREAM_MAX_BYTES},
        "appended": {
            "turns": counters.appended_turns.load(Ordering::Relaxed),
            "nodes": counters.appended_nodes.load(Ordering::Relaxed),
        },
        "dropped": {
            "queue_full_turns": counters.dropped_queue_turns.load(Ordering::Relaxed),
            "queue_full_nodes": counters.dropped_queue_nodes.load(Ordering::Relaxed),
            "cap_turns": counters.dropped_cap_turns.load(Ordering::Relaxed),
            "cap_nodes": counters.dropped_cap_nodes.load(Ordering::Relaxed),
            "refused": counters.refused.load(Ordering::Relaxed),
            "disk_errors": counters.disk_errors.load(Ordering::Relaxed),
        },
        "backlog": backlog,
        "sinks": sinks,
        "next_seq": next_seq,
        "fsynced_seq": fsynced_seq,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(seq: u64, ts_ms: u64, subject: &str, payload: &str) -> Record {
        Record {
            seq,
            ts_ms,
            source: Source::Turns,
            subject: Arc::from(subject),
            producer: Arc::from("AI.sales@worker1"),
            payload: Arc::from(payload.as_bytes().to_vec()),
        }
    }

    fn shared_with(records: Vec<Record>) -> Shared {
        let mut shared = Shared {
            records: VecDeque::new(),
            turn_bytes: 0,
            node_bytes: 0,
            next_seq: records.last().map(|r| r.seq + 1).unwrap_or(1),
            fsynced_seq: 0,
            sinks: HashMap::new(),
            role: "worker".to_string(),
            hive_id: "worker1".to_string(),
        };
        for r in records {
            shared.turn_bytes += r.size();
            shared.records.push_back(r);
        }
        shared
            .sinks
            .insert("SY.storage@motherbee".to_string(), SinkState::default());
        shared
    }

    #[test]
    fn records_round_trip_and_a_torn_tail_is_cut() {
        let a = record(1, 10, SUBJECT_STORAGE_TURNS, r#"{"a":1}"#);
        let b = record(2, 11, SUBJECT_STORAGE_TURNS, r#"{"b":2}"#);
        let mut bytes = encode_record(&a);
        bytes.extend(encode_record(&b));
        let whole = bytes.len();
        bytes.extend_from_slice(&encode_record(&record(3, 12, SUBJECT_STORAGE_TURNS, "{}"))[..7]);
        let (decoded, valid) = decode_records(&bytes);
        assert_eq!(decoded, vec![a, b]);
        assert_eq!(valid, whole);
        // A flipped byte fails the checksum: decoding stops there.
        let mut corrupt = encode_record(&record(1, 10, SUBJECT_STORAGE_TURNS, "{}"));
        let last = corrupt.len() - 1;
        corrupt[last] ^= 0xff;
        assert_eq!(decode_records(&corrupt).0, Vec::new());
    }

    #[test]
    fn only_allowed_producers_append() {
        assert!(!subject_allowed(
            SUBJECT_STORAGE_TURNS,
            "AI.sales@worker1",
            "worker1"
        ));
        assert!(subject_allowed(
            "storage.cognition.threads",
            "SY.cognition@worker1",
            "worker1"
        ));
        assert!(!subject_allowed(
            "storage.cognition.threads",
            "AI.sales@worker1",
            "worker1"
        ));
        assert!(!subject_allowed(
            "storage.cognition.threads",
            "SY.cognition@motherbee",
            "worker1"
        ));
        assert!(subject_allowed(
            SUBJECT_STORAGE_USAGE,
            "AI.sales@worker1",
            "worker1"
        ));
        assert!(!subject_allowed(
            "storage.anything",
            "AI.sales@worker1",
            "worker1"
        ));
    }

    #[test]
    fn a_batch_takes_records_in_order_and_skips_what_it_must() {
        let now = 1_000_000_000;
        let fresh = now - 1_000;
        let stale = now - (STREAM_MAX_AGE_SECS + 1) * 1000;
        let big = "x".repeat(STREAM_BATCH_MAX_BYTES);
        let mut shared = shared_with(vec![
            record(1, stale, SUBJECT_STORAGE_TURNS, "{}"),
            record(2, fresh, SUBJECT_STORAGE_TURNS, &format!("\"{big}\"")),
            record(3, fresh, SUBJECT_STORAGE_TURNS, r#"{"ok":3}"#),
            record(4, fresh, SUBJECT_STORAGE_TURNS, r#"{"ok":4}"#),
        ]);
        for attempt in 0..2 {
            match plan_batch(&mut shared, "SY.storage@motherbee", now) {
                BatchPlan::Send {
                    records,
                    last_seq,
                    scanned_up_to,
                } => {
                    assert_eq!(
                        records.iter().map(|r| r.seq).collect::<Vec<_>>(),
                        vec![3, 4]
                    );
                    assert_eq!((last_seq, scanned_up_to), (4, 4));
                }
                other => panic!("unexpected plan: {other:?}"),
            }
            let state = shared.sinks.get_mut("SY.storage@motherbee").unwrap();
            // The cursor passed what was lost ahead of the batch, counted once: a batch that is
            // not acked and planned again counts nothing more.
            assert_eq!(state.cursor, 2, "attempt {attempt}");
            assert_eq!((state.expired, state.oversize), (1, 1), "attempt {attempt}");
            assert_eq!(state.in_flight_up_to, 4);
            state.in_flight_up_to = 0;
        }
    }

    #[test]
    fn a_record_lost_after_the_first_of_a_batch_is_passed_by_the_next_plan() {
        let now = 1_000_000_000;
        let stale = now - (STREAM_MAX_AGE_SECS + 1) * 1000;
        let mut shared = shared_with(vec![
            record(1, now, SUBJECT_STORAGE_TURNS, "{}"),
            record(2, stale, SUBJECT_STORAGE_TURNS, "{}"),
            record(3, now, SUBJECT_STORAGE_TURNS, "{}"),
        ]);
        let sink = "SY.storage@motherbee";
        match plan_batch(&mut shared, sink, now) {
            BatchPlan::Send { records, .. } => {
                assert_eq!(records.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![1])
            }
            other => panic!("unexpected plan: {other:?}"),
        }
        assert_eq!(shared.sinks[sink].expired, 0, "not passed yet");
        let state = shared.sinks.get_mut(sink).unwrap();
        state.cursor = 1;
        state.in_flight_up_to = 0;
        match plan_batch(&mut shared, sink, now) {
            BatchPlan::Send { records, .. } => {
                assert_eq!(records.iter().map(|r| r.seq).collect::<Vec<_>>(), vec![3])
            }
            other => panic!("unexpected plan: {other:?}"),
        }
        assert_eq!(shared.sinks[sink].expired, 1);
    }

    #[test]
    fn a_record_that_leaves_before_its_sink_took_it_is_counted_once() {
        let now = 1_000_000_000;
        let stale = now - (STREAM_MAX_AGE_SECS + 1) * 1000;
        let sink = "SY.storage@motherbee";
        let mut shared = shared_with(vec![
            record(1, stale, SUBJECT_STORAGE_TURNS, "{}"),
            record(2, now, SUBJECT_STORAGE_TURNS, "{}"),
        ]);
        // In flight: it stays until its ack (or the failure) says whether the sink took it.
        shared.sinks.get_mut(sink).unwrap().in_flight_up_to = 1;
        assert_eq!(shared.trim(now), 1);
        assert_eq!(shared.sinks[sink].expired, 0);
        shared.sinks.get_mut(sink).unwrap().in_flight_up_to = 0;
        assert_eq!(shared.trim(now), 2);
        assert_eq!(shared.sinks[sink].expired, 1);
        // The next plan starts past it and counts nothing more.
        assert!(matches!(
            plan_batch(&mut shared, sink, now),
            BatchPlan::Send { .. }
        ));
        assert_eq!(shared.sinks[sink].expired, 1);
    }

    #[test]
    fn a_batch_stops_at_its_record_count() {
        let now = 1_000_000_000;
        let records: Vec<Record> = (1..=(STREAM_BATCH_MAX_RECORDS as u64 + 10))
            .map(|seq| record(seq, now, SUBJECT_STORAGE_TURNS, "{}"))
            .collect();
        let mut shared = shared_with(records);
        match plan_batch(&mut shared, "SY.storage@motherbee", now) {
            BatchPlan::Send { records, .. } => {
                assert_eq!(records.len(), STREAM_BATCH_MAX_RECORDS)
            }
            other => panic!("unexpected plan: {other:?}"),
        }
    }

    fn ack(from: &str, up_to_seq: u64) -> Message {
        Message {
            routing: Routing {
                src: "uuid".to_string(),
                src_l2_name: Some(from.to_string()),
                dst: Destination::Unicast("stream".to_string()),
                ttl: 8,
                trace_id: "t".to_string(),
            },
            meta: Meta {
                msg_type: SYSTEM_KIND.to_string(),
                msg: Some(MSG_STREAM_ACK.to_string()),
                ..Meta::default()
            },
            payload: json!({"up_to_seq": up_to_seq}),
        }
    }

    #[test]
    fn only_the_sinks_ack_for_what_was_sent_moves_its_cursor() {
        let sink = "SY.storage@motherbee";
        assert_eq!(accept_ack(&ack(sink, 9), sink, 4, 9, 11), Some(11));
        assert_eq!(accept_ack(&ack(sink, 7), sink, 4, 9, 11), Some(7));
        assert_eq!(
            accept_ack(&ack("AI.sales@worker1", 9), sink, 4, 9, 11),
            None
        );
        assert_eq!(accept_ack(&ack(sink, 50), sink, 4, 9, 11), None);
        assert_eq!(accept_ack(&ack(sink, 3), sink, 4, 9, 11), None);
    }

    #[test]
    fn records_are_trimmed_once_taken_or_past_the_window() {
        let now = 1_000_000_000;
        let mut shared = shared_with(vec![
            record(1, now, SUBJECT_STORAGE_TURNS, "{}"),
            record(2, now, SUBJECT_STORAGE_TURNS, "{}"),
            record(
                3,
                now - (STREAM_MAX_AGE_SECS + 1) * 1000,
                SUBJECT_STORAGE_TURNS,
                "{}",
            ),
        ]);
        assert_eq!(shared.trim(now), 1, "nothing taken yet");
        shared.sinks.get_mut("SY.storage@motherbee").unwrap().cursor = 1;
        assert_eq!(shared.trim(now), 2);
        assert_eq!(shared.records.len(), 2);
    }

    #[test]
    fn past_the_cap_new_records_are_dropped() {
        let shared = shared_with(Vec::new());
        let node_cap = STREAM_MAX_BYTES * STREAM_NODE_APPENDS_SHARE_PERCENT / 100;
        assert!(shared.fits(Source::Nodes, node_cap));
        assert!(!shared.fits(Source::Nodes, node_cap + 1));
        assert!(shared.fits(Source::Turns, STREAM_MAX_BYTES - node_cap));
        assert!(!shared.fits(Source::Turns, STREAM_MAX_BYTES - node_cap + 1));
    }

    #[test]
    fn a_restart_keeps_the_window_and_never_trusts_a_cursor_past_the_disk() {
        let dir = std::env::temp_dir().join(format!("fluxbee-stream-{}", uuid::Uuid::new_v4()));
        let sinks = vec!["SY.storage@motherbee".to_string()];
        let now = now_ms();
        {
            let recovered = recover(&dir, &sinks).unwrap();
            let mut disk = recovered.disk.unwrap();
            for seq in 1..=3 {
                disk.append(&record(seq, now, SUBJECT_STORAGE_TURNS, r#"{"n":1}"#))
                    .unwrap();
            }
            disk.fsync().unwrap();
            let mut cursors = HashMap::new();
            cursors.insert("SY.storage@motherbee".to_string(), 9);
            cursors.insert("SY.gone@motherbee".to_string(), 2);
            disk.save_state(77, 4, &cursors).unwrap();
        }
        let recovered = recover(&dir, &sinks).unwrap();
        assert_eq!(recovered.records.len(), 3);
        assert_eq!(recovered.next_seq, 4);
        assert_eq!(recovered.cursors.get("SY.storage@motherbee"), Some(&3));
        assert!(!recovered.cursors.contains_key("SY.gone@motherbee"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_newest_segment_goes_only_when_nothing_is_left() {
        let dir = std::env::temp_dir().join(format!("fluxbee-stream-{}", uuid::Uuid::new_v4()));
        let now = now_ms();
        let recovered = recover(&dir, &[]).unwrap();
        let mut disk = recovered.disk.unwrap();
        disk.append(&record(1, now, SUBJECT_STORAGE_TURNS, "{}"))
            .unwrap();
        disk.trim(100, false);
        assert_eq!(disk.segments.len(), 1, "the newest segment stays");
        let path = disk.segments[0].path.clone();
        disk.save_state(77, 2, &HashMap::new()).unwrap();
        disk.trim(100, true);
        assert!(
            disk.segments.is_empty() && !path.exists(),
            "nothing left to deliver"
        );
        disk.append(&record(2, now, SUBJECT_STORAGE_TURNS, "{}"))
            .unwrap();
        assert_eq!(
            disk.segments.len(),
            1,
            "the next record starts a new segment"
        );
        let recovered = recover(&dir, &[]).unwrap();
        assert_eq!(recovered.next_seq, 3);
        assert_eq!(recovered.records.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_record_past_the_queued_bytes_is_dropped() {
        let (handle, observed) = StreamHandle::observed();
        handle.append_turn(
            "AI.sales@worker1",
            vec![b'x'; STREAM_QUEUE_MAX_BYTES as usize + 1],
        );
        handle.append_turn("AI.sales@worker1", b"{}".to_vec());
        assert_eq!(observed.drain().len(), 1);
        assert_eq!(
            handle.counters.dropped_queue_turns.load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn cognition_copies_go_to_the_hives_own_cognition_only() {
        assert_eq!(
            sinks_for(SUBJECT_COGNITION_TURNS, "worker", "worker1"),
            vec!["SY.cognition@worker1".to_string()]
        );
        assert_eq!(
            sinks_for(SUBJECT_COGNITION_TURNS, "motherbee", "motherbee"),
            vec!["SY.cognition@motherbee".to_string()]
        );
        assert!(sinks_for(SUBJECT_COGNITION_TURNS, "ingress", "ingress1").is_empty());
        assert!(all_sinks("worker", "worker1").contains(&"SY.cognition@worker1".to_string()));
        assert!(!subject_allowed(
            SUBJECT_COGNITION_TURNS,
            "SY.cognition@worker1",
            "worker1"
        ));
        assert!(subject_allowed(
            fluxbee_sdk::stream::SUBJECT_STORAGE_COGNITION_BATCH,
            "SY.cognition@worker1",
            "worker1"
        ));
    }

    #[test]
    fn the_epoch_survives_a_restart_and_changes_when_the_state_is_lost() {
        let dir = std::env::temp_dir().join(format!("fluxbee-stream-{}", uuid::Uuid::new_v4()));
        let first = recover(&dir, &[]).unwrap();
        let disk = first.disk.unwrap();
        disk.save_state(first.epoch, 1, &HashMap::new()).unwrap();
        assert_eq!(recover(&dir, &[]).unwrap().epoch, first.epoch);
        fs::remove_file(dir.join(STATE_FILE)).unwrap();
        assert_ne!(recover(&dir, &[]).unwrap().epoch, first.epoch);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_alarm_reports_only_what_grew() {
        let before = AlarmSnapshot::default();
        assert_eq!(before.since(&before), None);
        let after = AlarmSnapshot {
            dropped_cap: 3,
            expired: 2,
            ..AlarmSnapshot::default()
        };
        let line = after.since(&before).unwrap();
        assert!(line.contains("dropped (cap): 3") && line.contains("expired: 2"));
        assert!(!line.contains("refused"));
    }
}
