use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use uuid::Uuid;

use json_router::shm::{RouteEntry, ShmReader};

fn main() {
    let args: Vec<String> = env::args().collect();
    let router_name = parse_arg(&args, "--name").or_else(|| env::var("JSR_ROUTER_NAME").ok());
    let config_dir = parse_arg(&args, "--config")
        .map(PathBuf::from)
        .or_else(|| env::var("JSR_CONFIG_DIR").ok().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("/etc/json-router"));

    if let Some(router_name) = router_name {
        show_router(&config_dir, &router_name);
    } else {
        show_all_routers(&config_dir);
    }
}

fn parse_arg(args: &[String], flag: &str) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == flag {
            return iter.next().cloned();
        }
    }
    None
}

fn bytes_to_string(buf: &[u8], len: usize) -> String {
    let len = len.min(buf.len());
    String::from_utf8_lossy(&buf[..len]).to_string()
}

fn count_active_nodes(nodes: &[json_router::shm::nodes::NodeEntry]) -> usize {
    nodes.iter().filter(|node| node.is_active()).count()
}

fn count_active_routes(routes: &[RouteEntry]) -> usize {
    routes.iter().filter(|route| route.is_active()).count()
}

#[derive(Debug, Deserialize)]
struct RouterConfigFile {
    #[serde(default)]
    paths: PathsConfigFile,
}

#[derive(Debug, Deserialize, Default)]
struct PathsConfigFile {
    state_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct IdentityFile {
    shm: IdentityShm,
}

#[derive(Debug, Deserialize)]
struct IdentityShm {
    name: String,
}

fn resolve_shm_name(config_dir: &Path, router_name: &str) -> Result<String, String> {
    let router_config_path = config_dir
        .join("routers")
        .join(router_name)
        .join("config.yaml");
    if !router_config_path.exists() {
        return Err(format!("router not found: {}", router_name));
    }

    let config_data = fs::read_to_string(&router_config_path)
        .map_err(|err| format!("read config.yaml failed: {}", err))?;
    let config: RouterConfigFile =
        serde_yaml::from_str(&config_data).map_err(|err| format!("parse config.yaml failed: {}", err))?;

    let state_dir = config
        .paths
        .state_dir
        .unwrap_or_else(|| PathBuf::from("./state"));
    let identity_path = state_dir.join(router_name).join("identity.yaml");
    if !identity_path.exists() {
        return Err(format!("identity not found for router: {}", router_name));
    }

    let identity_data = fs::read_to_string(&identity_path)
        .map_err(|err| format!("read identity.yaml failed: {}", err))?;
    let identity: IdentityFile =
        serde_yaml::from_str(&identity_data).map_err(|err| format!("parse identity.yaml failed: {}", err))?;
    Ok(identity.shm.name)
}

fn show_router(config_dir: &Path, router_name: &str) {
    let shm_name = match resolve_shm_name(config_dir, router_name) {
        Ok(name) => name,
        Err(err) => {
            eprintln!("error: {}", err);
            return;
        }
    };

    let reader = match ShmReader::open_read_only(&shm_name) {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("error opening shm {}: {}", shm_name, err);
            return;
        }
    };

    let Some(snapshot) = reader.read_snapshot() else {
        eprintln!("failed to read snapshot for {}", router_name);
        return;
    };

    print_snapshot(router_name, &shm_name, snapshot);
}

fn show_all_routers(config_dir: &Path) {
    let routers_dir = config_dir.join("routers");
    let entries = match fs::read_dir(&routers_dir) {
        Ok(entries) => entries,
        Err(err) => {
            eprintln!("error: cannot read routers dir {}: {}", routers_dir.display(), err);
            return;
        }
    };

    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let router_name = entry.file_name().to_string_lossy().to_string();
        show_router(config_dir, &router_name);
        println!();
    }
}

fn print_snapshot(router_name: &str, shm_name: &str, snapshot: json_router::shm::ShmSnapshot) {
    let router_uuid = Uuid::from_slice(&snapshot.router_uuid).unwrap_or_else(|_| Uuid::nil());
    println!("router: {}", router_name);
    println!("shm: {}", shm_name);
    println!("router_uuid: {}", router_uuid);
    println!("heartbeat: {}", snapshot.heartbeat);
    println!("generation: {}", snapshot.generation);
    println!("nodes: {}", count_active_nodes(&snapshot.nodes));
    println!("routes: {}", count_active_routes(&snapshot.routes));
    println!();
    println!("active nodes:");
    for node in snapshot.nodes {
        if !node.is_active() {
            continue;
        }
        let name = bytes_to_string(&node.name, node.name_len as usize);
        let uuid = Uuid::from_slice(&node.uuid).unwrap_or_else(|_| Uuid::nil());
        println!("  {} {}", uuid, name);
    }
    println!();
    println!("active routes:");
    for route in snapshot.routes {
        if !route.is_active() {
            continue;
        }
        let prefix = bytes_to_string(&route.prefix, route.prefix_len as usize);
        let next_hop = Uuid::from_slice(&route.next_hop_router).unwrap_or_else(|_| Uuid::nil());
        println!(
            "  {} match={} type={} out_link={} admin={} metric={} next_hop={}",
            prefix,
            route.match_kind,
            route.route_type,
            route.out_link,
            route.admin_distance,
            route.metric,
            next_hop
        );
    }
}
