//! Log setup shared by every Rust binary of the platform.
//!
//! Colour codes only when stdout is a terminal (FINDINGS A-31). Under systemd stdout is the
//! journal, and `\x1b[33m WARN\x1b[0m` there made every `grep ' WARN '` over `journalctl` count
//! zero: a validation once read 0 errors that were there.

use std::io::IsTerminal;

/// Whether log lines carry ANSI colour codes: only when stdout is a terminal.
pub fn ansi() -> bool {
    std::io::stdout().is_terminal()
}

/// `tracing_subscriber::fmt()` with colour only on a terminal. Every binary starts its subscriber
/// here; a layered setup passes [`ansi`] to its `fmt::layer().with_ansi(..)` instead.
pub fn fmt() -> tracing_subscriber::fmt::SubscriberBuilder {
    tracing_subscriber::fmt().with_ansi(ansi())
}
