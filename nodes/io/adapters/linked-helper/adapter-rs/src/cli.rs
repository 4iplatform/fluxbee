use clap::{Parser, Subcommand};

/**
 * CLI surface for the Linked Helper adapter current phase.
 */
#[derive(Debug, Parser)]
#[command(name = "linked-helper-adapter")]
#[command(about = "Rust adapter for Linked Helper")]
pub struct AdapterCli {
    #[arg(long)]
    pub state_file: Option<String>,

    #[command(subcommand)]
    pub command: AdapterCommand,
}

#[derive(Debug, Subcommand)]
pub enum AdapterCommand {
    /// Bootstraps the adapter on first run and then enters the normal service loop.
    Start {
        #[arg(long)]
        cloud: Option<String>,
        #[arg(long)]
        token: Option<String>,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        device_hint: Option<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        partitions_root: Option<String>,
        #[arg(long)]
        interval_seconds: Option<u64>,
        /// Optional slow administrative re-sync interval (seconds). When set, the
        /// adapter contacts Cloud at least this often even with no pending work,
        /// so Cloud can push updates/admin changes to an otherwise-quiet adapter.
        /// Off by default (pure on-demand).
        #[arg(long)]
        admin_resync_seconds: Option<u64>,
        #[arg(long, default_value_t = false)]
        once: bool,
    },
    /// Enrolls one adapter installation against Fluxbee Cloud.
    Enroll {
        #[arg(long)]
        cloud: String,
        #[arg(long)]
        token: String,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long)]
        device_hint: Option<String>,
        #[arg(long)]
        version: Option<String>,
        #[arg(long, default_value_t = false)]
        force: bool,
    },
    /// Prints the stored adapter state.
    Status,
    /// Sends discovery using either manual data or a payload file.
    Discover {
        #[command(subcommand)]
        mode: DiscoverMode,
    },
    /// Sends one alive payload to Fluxbee Cloud.
    Alive {
        #[arg(long)]
        partitions_root: Option<String>,
    },
    /// Scans a Linked Helper Partitions directory and prints the discovery payload.
    Scan {
        #[arg(long)]
        partitions_root: String,
    },
    /// Scans a Linked Helper Partitions directory and sends discovery to Cloud.
    DiscoverScan {
        #[arg(long)]
        partitions_root: String,
    },
    /// Runs the adapter loop: continuous runtime poll to the node, on-demand
    /// administrative contact with Cloud.
    Run {
        #[arg(long)]
        partitions_root: Option<String>,
        #[arg(long)]
        interval_seconds: Option<u64>,
        /// Optional slow administrative re-sync interval (seconds). When set, the
        /// adapter contacts Cloud at least this often even with no pending work,
        /// so Cloud can push updates/admin changes to an otherwise-quiet adapter.
        /// Off by default (pure on-demand).
        #[arg(long)]
        admin_resync_seconds: Option<u64>,
        #[arg(long, default_value_t = false)]
        once: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum DiscoverMode {
    /// Sends one manually provided discovery item.
    Manual {
        #[arg(long)]
        instance_id: String,
        #[arg(long)]
        instance_path: Option<String>,
        #[arg(long)]
        account_display_name: Option<String>,
        #[arg(long)]
        account_email: Option<String>,
        #[arg(long)]
        account_fingerprint: Option<String>,
    },
    /// Sends one discovery payload already serialized in JSON.
    PayloadFile {
        #[arg(long)]
        payload_file: String,
    },
}
