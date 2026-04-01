use std::env;
use std::process;

use json_router::shm::{memory_shm_name_for_hive, MemoryRegionReader};
use serde_json::json;

type DynError = Box<dyn std::error::Error + Send + Sync>;

fn usage() -> ! {
    eprintln!("usage: cognition_shm_dump --hive <hive_id> [--thread-id <thread_id>]");
    process::exit(2);
}

fn required_arg(args: &[String], name: &str) -> String {
    let Some(index) = args.iter().position(|value| value == name) else {
        usage();
    };
    let Some(value) = args.get(index + 1) else {
        usage();
    };
    value.clone()
}

fn optional_arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|value| value == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn main() -> Result<(), DynError> {
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        usage();
    }

    let hive_id = required_arg(&args, "--hive");
    let thread_id = optional_arg(&args, "--thread-id");
    let shm_name = memory_shm_name_for_hive(&hive_id)?;
    let reader = MemoryRegionReader::open_read_only(&shm_name)?;
    let snapshot = reader.read_snapshot()?;

    if let Some(thread_id) = thread_id {
        let Some(entry) = snapshot
            .threads
            .iter()
            .find(|entry| entry.thread_id == thread_id)
        else {
            eprintln!("thread not found in {}: {}", shm_name, thread_id);
            process::exit(3);
        };
        println!("{}", serde_json::to_string_pretty(&entry)?);
        return Ok(());
    }

    let payload = json!({
        "shm_name": shm_name,
        "schema_version": snapshot.schema_version,
        "updated_at": snapshot.updated_at,
        "thread_count": snapshot.threads.len(),
        "threads": snapshot.threads
    });
    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}
