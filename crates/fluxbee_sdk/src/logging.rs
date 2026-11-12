//! Log setup shared by every Rust binary of the platform: [`fmt`] starts a binary's subscriber, and
//! [`layer`] gives the same output to a subscriber built of layers (SY.edge's, whose filter
//! reloads live).
//!
//! - **Colour only on a terminal** (FINDINGS A-31). Under systemd stdout is the journal, and
//!   `\x1b[33m WARN\x1b[0m` there made every `grep ' WARN '` over `journalctl` count zero: a
//!   validation once read 0 errors that were there.
//! - **Each record one line** (FINDINGS A-269). journald takes each line a service writes as an
//!   entry of its own, ends one at a NUL too, and splits a line longer than its `LineMax` (48K by
//!   default). So a field printed as it came (IO.cloud's `op = %op`, the router's node name) with
//!   a newline in it wrote a line of its sender's choosing into the journal: a `WARN`, or the
//!   router stream's loss alarm that `lab/ops.py health` sums. [`OneLine`] writes each record as
//!   one line of at most [`RECORD_MAX_BYTES`]: its control characters escaped as Rust's `Debug`
//!   shows them (`\n`, `\r`, `\t`, `\0`, `\u{1b}`), a longer record cut. A record with neither
//!   is written as the formatter made it.
//! - **Two forms of an escape.** A record's message (and an error logged as `&dyn Error`) reaches
//!   [`OneLine`] with its ESC, BEL, BS, FF and DEL already escaped in tracing-subscriber's own
//!   form: `\x1b`, `\x07`, `\x08`, `\x0c`, `\x7f`. So the journal shows a message's ESC as `\x1b`
//!   and a field's as `\u{1b}`: a search for one form misses the other.

use std::borrow::Cow;
use std::io::{self, IsTerminal, Write};

use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::format::{DefaultFields, Format};
use tracing_subscriber::fmt::{Layer, MakeWriter, SubscriberBuilder};

/// The most bytes one record writes, its newline included (FINDINGS A-269): a third of journald's
/// default `LineMax`, so the journal never splits a record. A longer record keeps its first bytes,
/// as many as leave room, never part of a character, an escape or a colour code, and ends with
/// `... (cut: N bytes)`, N the bytes it had.
pub const RECORD_MAX_BYTES: usize = 16 * 1024;

/// Where [`fmt`] and [`layer`] write: stdout, each record one line.
pub type StdoutLines = OneLine<fn() -> io::Stdout>;

/// `tracing_subscriber::fmt()` writing each record to stdout as one line ([`OneLine`]), with colour
/// only on a terminal. Every binary starts its subscriber here; one built of layers takes
/// [`layer`]. Give it no other writer (`with_writer`): its records would skip [`OneLine`].
pub fn fmt() -> SubscriberBuilder<DefaultFields, Format, LevelFilter, StdoutLines> {
    fmt_to(io::stdout as fn() -> io::Stdout, ansi())
}

/// [`fmt`]'s output as a layer, for a subscriber built of layers (SY.edge's, whose filter reloads
/// live). A binary never builds a `fmt::layer()` of its own: its records would skip [`OneLine`].
pub fn layer<S>() -> Layer<S, DefaultFields, Format, StdoutLines> {
    layer_to(io::stdout as fn() -> io::Stdout, ansi())
}

/// Whether log lines carry ANSI colour codes: only when stdout is a terminal.
fn ansi() -> bool {
    io::stdout().is_terminal()
}

/// [`fmt`] writing to `out`, with colour or without.
fn fmt_to<M>(
    out: M,
    colour: bool,
) -> SubscriberBuilder<DefaultFields, Format, LevelFilter, OneLine<M>>
where
    M: for<'w> MakeWriter<'w> + 'static,
{
    tracing_subscriber::fmt()
        .with_ansi(colour)
        .with_writer(OneLine::new(out, colour))
}

/// [`layer`] writing to `out`, with colour or without.
fn layer_to<S, M>(out: M, colour: bool) -> Layer<S, DefaultFields, Format, OneLine<M>>
where
    M: for<'w> MakeWriter<'w> + 'static,
{
    tracing_subscriber::fmt::layer()
        .with_ansi(colour)
        .with_writer(OneLine::new(out, colour))
}

/// A [`MakeWriter`] that writes each record as one line (FINDINGS A-269). The fmt layer asks it for
/// a writer for each record and writes the record into it; that writer, a [`RecordWriter`], writes
/// the record to `out` as one line when the layer drops it, as the record ends.
#[derive(Clone, Debug)]
pub struct OneLine<M> {
    out: M,
    colour: bool,
}

impl<M> OneLine<M> {
    /// Each record goes to `out` as one line. `colour`: the formatter writes ANSI colour codes;
    /// every colour code then stays as it is, a field's too, and no other escape does.
    pub fn new(out: M, colour: bool) -> Self {
        Self { out, colour }
    }
}

impl<'a, M: MakeWriter<'a>> MakeWriter<'a> for OneLine<M> {
    type Writer = RecordWriter<M::Writer>;

    fn make_writer(&'a self) -> Self::Writer {
        RecordWriter::new(self.out.make_writer(), self.colour)
    }

    fn make_writer_for(&'a self, meta: &tracing::Metadata<'_>) -> Self::Writer {
        RecordWriter::new(self.out.make_writer_for(meta), self.colour)
    }
}

/// One record on its way to its line. Whatever is written into it is the record, in as many writes
/// as it takes; it goes to `out` once, as one line, when the writer is dropped. A flush never
/// writes it, so no flush can cut a record into two lines.
pub struct RecordWriter<W: Write> {
    out: W,
    record: Vec<u8>,
    colour: bool,
}

impl<W: Write> RecordWriter<W> {
    fn new(out: W, colour: bool) -> Self {
        Self {
            out,
            record: Vec::new(),
            colour,
        }
    }
}

impl<W: Write> Write for RecordWriter<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.record.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl<W: Write> Drop for RecordWriter<W> {
    fn drop(&mut self) {
        if self.record.is_empty() {
            return;
        }
        // A line that cannot be written has nowhere else to say so: the journal is the place.
        let _ = self.out.write_all(&one_line(&self.record, self.colour));
    }
}

/// `record`, as the formatter made it, as the one line written for it: its newline last, at most
/// [`RECORD_MAX_BYTES`].
/// - Every control character in it (`char::is_control`: C0, DEL and C1) but its final newline is
///   escaped as Rust's `Debug` shows it: a newline or a NUL would end the journal's line there,
///   and a carriage return or an escape would move a terminal's cursor. (A message's ESC, BEL,
///   BS, FF and DEL come already escaped, by tracing-subscriber: see the module's doc.)
/// - With `colour`, every ANSI colour code in it (`ESC [`, digits and `;`, `m`) stays as it is:
///   the formatter's, and a field's too, which on the terminal can recolour or hide (`ESC [8m`)
///   the rest of its line; nothing here tells them apart. A cursor's move, a line's erase and
///   every other escape are escaped.
/// - A longer record is cut at the end of a piece (a character, an escape or a colour code) and
///   ends with `... (cut: N bytes)`, N the bytes it had.
///
/// A record that already is such a line comes back as it came.
fn one_line(record: &[u8], colour: bool) -> Cow<'_, [u8]> {
    let ends_with_newline = record.ends_with(b"\n");
    let text = String::from_utf8_lossy(record);
    let body = text.strip_suffix('\n').unwrap_or(&text);
    if ends_with_newline
        && record.len() <= RECORD_MAX_BYTES
        && matches!(text, Cow::Borrowed(_))
        && !body.chars().any(char::is_control)
    {
        return Cow::Borrowed(record);
    }

    let had = record.len() - usize::from(ends_with_newline);
    let marker = format!("... (cut: {had} bytes)");
    let room = RECORD_MAX_BYTES - 1;
    let mut line = String::with_capacity(body.len().min(room) + 1);
    // The length of `line` at the last piece's end that leaves room for the marker.
    let mut kept = 0;
    let mut cut = false;
    let mut rest = body;
    while let Some(c) = rest.chars().next() {
        let before = line.len();
        let code = if colour { colour_code_len(rest) } else { None };
        let taken = match code {
            Some(len) => {
                line.push_str(&rest[..len]);
                len
            }
            None if c.is_control() => {
                line.extend(c.escape_debug());
                c.len_utf8()
            }
            None => {
                line.push(c);
                c.len_utf8()
            }
        };
        if line.len() > room {
            line.truncate(before);
            cut = true;
            break;
        }
        rest = &rest[taken..];
        if line.len() + marker.len() <= room {
            kept = line.len();
        }
    }
    if cut {
        line.truncate(kept);
        line.push_str(&marker);
    }
    line.push('\n');
    Cow::Owned(line.into_bytes())
}

/// The length of the ANSI colour code `text` starts with (`ESC [`, digits and `;`, then `m`), if it
/// starts with one.
fn colour_code_len(text: &str) -> Option<usize> {
    let params = text.strip_prefix("\u{1b}[")?;
    let len = params
        .bytes()
        .take_while(|byte| byte.is_ascii_digit() || *byte == b';')
        .count();
    params[len..].starts_with('m').then_some(len + 3)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing_subscriber::layer::SubscriberExt;

    /// What a subscriber wrote, as the journal takes it. Each test logs through callsites of its
    /// own, under a subscriber set for its thread alone, so no other test's thread caches them as
    /// unwanted first.
    #[derive(Clone, Default)]
    struct Journal(Arc<Mutex<Vec<u8>>>);

    impl Write for Journal {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Journal {
        /// A writer for each record, as stdout is.
        fn out(&self) -> impl Fn() -> Journal + 'static {
            let journal = self.clone();
            move || journal.clone()
        }

        fn text(&self) -> String {
            let bytes = self
                .0
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            String::from_utf8(bytes).expect("the journal's text")
        }
    }

    /// A Cloud request's `op` as its sender could write it: a newline, then the router stream's
    /// loss alarm at WARN, as a line of its own would show it, which `lab/ops.py health` sums.
    const FORGED_OP: &str = "list_nodes\n2026-11-12T03:00:00.000000Z  WARN json_router::router::stream: router stream lost records in the last 30 s: dropped (queue full): 0, dropped (cap): 0, refused appends: 0, expired: 99999, oversize: 0, disk errors: 0";

    /// The forged text as it shows inside its record's line: its newline escaped.
    const FORGED_OP_ESCAPED: &str = r"list_nodes\n2026-11-12T03:00:00.000000Z  WARN json_router::router::stream: router stream lost records";

    /// The journal holds exactly one line, which ends it, and no line opens with the forged one.
    fn assert_one_line(text: &str) {
        assert_eq!(text.matches('\n').count(), 1, "one line: {text:?}");
        assert!(text.ends_with('\n'), "{text:?}");
        assert!(
            !text
                .lines()
                .any(|line| line.starts_with("2026-11-12T03:00:00")),
            "a forged line: {text:?}"
        );
    }

    fn line(record: &str, colour: bool) -> String {
        String::from_utf8(one_line(record.as_bytes(), colour).into_owned()).expect("UTF-8")
    }

    /// A-269: a field with a newline and a forged WARN after it, as IO.cloud logs the Cloud's `op`
    /// (`op = %op`), stays inside its record's one line, escaped, through what `fmt` builds.
    #[test]
    fn a_field_with_a_newline_and_a_forged_warn_stays_one_escaped_line() {
        let journal = Journal::default();
        let subscriber = fmt_to(journal.out(), false).finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(
                trace_id = %"trace-1",
                op = %FORGED_OP,
                "IO.cloud received a Cloud request"
            );
        });
        let text = journal.text();
        assert_one_line(&text);
        assert!(text.contains(" INFO "), "{text:?}");
        assert!(
            text.contains("IO.cloud received a Cloud request"),
            "{text:?}"
        );
        assert!(
            text.contains(&format!("op={FORGED_OP_ESCAPED}")),
            "{text:?}"
        );
    }

    /// The message and a span's fields are the record's too: each stays inside its line.
    #[test]
    fn a_message_and_a_span_field_with_a_newline_stay_in_their_line() {
        let journal = Journal::default();
        let subscriber = fmt_to(journal.out(), false).finish();
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!("cloud_request", op = %FORGED_OP);
            let _entered = span.enter();
            tracing::warn!("IO.cloud refused {FORGED_OP}");
        });
        let text = journal.text();
        assert_one_line(&text);
        assert_eq!(text.matches(FORGED_OP_ESCAPED).count(), 2, "{text:?}");
    }

    /// SY.edge builds its subscriber of layers, its filter reloadable: `layer` writes as `fmt`.
    #[test]
    fn the_layer_writes_each_record_as_one_line_too() {
        let journal = Journal::default();
        let (filter, _reload) =
            tracing_subscriber::reload::Layer::new(tracing_subscriber::EnvFilter::new("info"));
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(layer_to(journal.out(), false));
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(op = %FORGED_OP, "IO.cloud received a Cloud request");
        });
        let text = journal.text();
        assert_one_line(&text);
        assert!(
            text.contains(&format!("op={FORGED_OP_ESCAPED}")),
            "{text:?}"
        );
    }

    /// A record longer than 16 KiB is cut there, its line naming the bytes it had.
    #[test]
    fn a_record_past_16_kib_is_cut_there() {
        let journal = Journal::default();
        let payload = "é".repeat(12_000);
        let subscriber = fmt_to(journal.out(), false).finish();
        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(payload = %payload, "a long record");
        });
        let text = journal.text();
        assert_eq!(text.matches('\n').count(), 1, "one line");
        // 'é' is 2 bytes: what is kept fills the room but at most a byte.
        assert!(
            (RECORD_MAX_BYTES - 1..=RECORD_MAX_BYTES).contains(&text.len()),
            "{} bytes",
            text.len()
        );
        let (kept, had) = text
            .trim_end()
            .rsplit_once("... (cut: ")
            .expect("the cut's marker");
        let had: usize = had
            .strip_suffix(" bytes)")
            .and_then(|had| had.parse().ok())
            .expect("the bytes it had");
        assert!(had > payload.len(), "it had {had} bytes");
        assert!(kept.contains(" INFO "), "{kept:?}");
        assert!(kept.contains("a long record payload=éé"), "{kept:?}");
        assert!(kept.ends_with("éé"), "whole characters: {kept:?}");
    }

    #[test]
    fn a_record_with_no_control_character_is_written_as_it_came() {
        let record = "2026-11-12T03:00:00.000000Z  INFO io_cloud: IO.cloud received a Cloud request op=list_nodes path=\"C:\\\\x\" ok=ü\n";
        assert!(matches!(
            one_line(record.as_bytes(), false),
            Cow::Borrowed(_)
        ));
        assert_eq!(line(record, false), record);
    }

    #[test]
    fn every_control_character_is_escaped_as_debug_shows_it() {
        let record = "a\nb\rc\td\0e\u{1b}[1Af\u{7}g\u{7f}h\u{85}i\u{9b}j\n";
        assert_eq!(
            line(record, false),
            "a\\nb\\rc\\td\\0e\\u{1b}[1Af\\u{7}g\\u{7f}h\\u{85}i\\u{9b}j\n"
        );
    }

    #[test]
    fn a_record_without_its_newline_gets_one_and_any_other_is_escaped() {
        assert_eq!(line("x", false), "x\n");
        assert_eq!(line("x\n\n", false), "x\\n\n");
        assert_eq!(line("x\r\n", false), "x\\r\n");
    }

    /// On a terminal every colour code stays, the formatter's and a field's alike; no other escape
    /// does (a cursor's move, a line's erase, a title's set), and in the journal none does.
    #[test]
    fn on_a_terminal_colour_codes_stay_and_no_other_escape_does() {
        let record = "\u{1b}[2m2026-11-12T03:00:00.000000Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m x=1\u{1b}[1A\u{1b}[2K\u{1b}]0;t\u{7}\u{1b}[1;31mred\u{1b}[0m\u{1b}[\n";
        assert_eq!(
            line(record, true),
            "\u{1b}[2m2026-11-12T03:00:00.000000Z\u{1b}[0m \u{1b}[32m INFO\u{1b}[0m x=1\\u{1b}[1A\\u{1b}[2K\\u{1b}]0;t\\u{7}\u{1b}[1;31mred\u{1b}[0m\\u{1b}[\n"
        );
        assert!(!line(record, false).contains('\u{1b}'));
    }

    /// What the docs say a reader finds, through what `fmt` builds: a message's ESC, BEL, BS, FF
    /// and DEL in tracing-subscriber's form (`\x1b`), its newline and a field's control characters
    /// in `Debug`'s (`\u{1b}`); on a terminal a field's colour code passes, its other escapes not.
    #[test]
    fn a_message_and_a_field_show_their_escapes_as_the_docs_say() {
        for colour in [false, true] {
            let journal = Journal::default();
            let subscriber = fmt_to(journal.out(), colour).finish();
            tracing::subscriber::with_default(subscriber, || {
                tracing::info!(
                    f = %"x\u{1b}[8my\u{7}z\u{1b}[2K",
                    "m \u{1b}[31m \u{7} \u{8} \u{c} \u{7f} \n end"
                );
            });
            let text = journal.text();
            assert_eq!(text.matches('\n').count(), 1, "{colour}: {text:?}");
            assert!(
                text.contains(r"m \x1b[31m \x07 \x08 \x0c \x7f \n end"),
                "{colour}: {text:?}"
            );
            let field = if colour {
                "x\u{1b}[8my\\u{7}z\\u{1b}[2K"
            } else {
                r"x\u{1b}[8my\u{7}z\u{1b}[2K"
            };
            assert!(text.contains(field), "{colour}: {text:?}");
        }
    }

    /// The cut keeps whole pieces, as many as leave room for its marker: never part of an escape,
    /// a character or a colour code.
    #[test]
    fn a_long_record_is_cut_after_a_whole_piece() {
        for (piece, colour) in [
            ("a", false),
            ("é", false),
            ("\u{1}", false),
            ("\u{1b}[1;31m", true),
        ] {
            let record = format!("{}\n", piece.repeat(RECORD_MAX_BYTES));
            let out = line(&record, colour);
            let marker = format!("... (cut: {} bytes)\n", record.len() - 1);
            let kept = out.strip_suffix(&marker).expect("the cut's marker");
            let shown = line(piece, colour);
            let shown = shown.trim_end_matches('\n');
            assert_eq!(kept, shown.repeat(kept.len() / shown.len()), "{piece:?}");
            assert!(out.len() <= RECORD_MAX_BYTES, "{piece:?}: {}", out.len());
            assert!(
                out.len() + shown.len() > RECORD_MAX_BYTES,
                "{piece:?}: room for one more"
            );
        }
    }

    #[test]
    fn a_record_of_16_kib_stays_whole_and_one_byte_more_is_cut() {
        let whole = format!("{}\n", "a".repeat(RECORD_MAX_BYTES - 1));
        assert_eq!(line(&whole, false), whole);
        let longer = format!("{}\n", "a".repeat(RECORD_MAX_BYTES));
        let out = line(&longer, false);
        assert_eq!(out.len(), RECORD_MAX_BYTES);
        assert!(out.ends_with(&format!("... (cut: {RECORD_MAX_BYTES} bytes)\n")));
    }

    /// The record is what the layer wrote into its writer, however many writes it took: a newline
    /// at the end of one write does not end the line.
    #[test]
    fn a_record_written_in_pieces_is_still_one_line() {
        let journal = Journal::default();
        let lines = OneLine::new(journal.out(), false);
        {
            let mut writer = lines.make_writer();
            writer
                .write_all(b"2026-11-12T03:00:01.000000Z  INFO io_cloud: op=list_nodes\n")
                .expect("write");
            writer
                .write_all(b"2026-11-12T03:00:00.000000Z  WARN forged\n")
                .expect("write");
            writer.flush().expect("flush");
            assert_eq!(journal.text(), "", "nothing before the record ends");
        }
        assert_eq!(
            journal.text(),
            "2026-11-12T03:00:01.000000Z  INFO io_cloud: op=list_nodes\\n2026-11-12T03:00:00.000000Z  WARN forged\n"
        );
    }

    #[test]
    fn a_writer_that_got_nothing_writes_nothing() {
        let journal = Journal::default();
        drop(OneLine::new(journal.out(), false).make_writer());
        assert_eq!(journal.text(), "");
    }
}
