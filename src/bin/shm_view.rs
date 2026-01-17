use std::env;

use uuid::Uuid;

use json_router::shm::{RouteEntry, ShmReader};

fn main() {
    let args: Vec<String> = env::args().collect();
    let shm_name = parse_name(&args).or_else(|| env::var("JSR_SHM_NAME").ok());
    let Some(shm_name) = shm_name else {
        eprintln!("usage: shm_view --name <shm_name>");
        eprintln!("env: JSR_SHM_NAME=/jsr-<uuid>");
        std::process::exit(1);
    };

    let reader = match ShmReader::open_read_only(&shm_name) {
        Ok(reader) => reader,
        Err(err) => {
            eprintln!("error opening shm {}: {}", shm_name, err);
            std::process::exit(1);
        }
    };

    let Some(snapshot) = reader.read_snapshot() else {
        eprintln!("failed to read snapshot");
        std::process::exit(1);
    };

    let router_uuid = Uuid::from_slice(&snapshot.router_uuid).unwrap_or_else(|_| Uuid::nil());
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

fn parse_name(args: &[String]) -> Option<String> {
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--name" {
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
