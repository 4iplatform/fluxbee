pub mod routes;
pub mod nodes;
pub mod routers;

pub use crate::shm::nodes::NodeEntry;
pub use crate::shm::routes::RouteEntry;

use std::ffi::CString;
use std::io;
use std::mem::{align_of, size_of};
use std::sync::atomic::{self, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use memmap2::{Mmap, MmapMut, MmapOptions};
use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::sys::mman::shm_open;
use nix::sys::stat::{fstat, Mode};
use nix::unistd::ftruncate;
#[cfg(target_os = "linux")]
use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use serde_json;
use uuid::Uuid;


pub const SHM_MAGIC: u32 = 0x4A535352;
pub const SHM_VERSION: u32 = 1;
pub const SHM_NAME_PREFIX: &str = "/jsr-";
#[cfg(target_os = "macos")]
const SHM_NAME_LIMIT: usize = 31;
#[cfg(not(target_os = "macos"))]
const SHM_NAME_LIMIT: usize = SHM_NAME_MAX_LEN;

pub const MAX_NODES: u32 = 1024;
pub const MAX_ROUTES: u32 = 256;
pub const MAX_PEERS: usize = 16;

pub const NAME_MAX_LEN: usize = 256;
pub const ISLAND_ID_MAX_LEN: usize = 64;
pub const SHM_NAME_MAX_LEN: usize = 64;

pub const FLAG_ACTIVE: u16 = 0x0001;
pub const FLAG_DELETED: u16 = 0x0002;

pub const MATCH_EXACT: u8 = 0;
pub const MATCH_PREFIX: u8 = 1;
pub const MATCH_GLOB: u8 = 2;

pub const ROUTE_CONNECTED: u8 = 0;
pub const ROUTE_STATIC: u8 = 1;
pub const ROUTE_LSA: u8 = 2;

pub const AD_CONNECTED: u16 = 0;
pub const AD_STATIC: u16 = 1;
pub const AD_LSA: u16 = 10;

pub const LINK_LOCAL: u32 = 0;

pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const HEARTBEAT_STALE_MS: u64 = 30_000;

const REGION_ALIGNMENT: usize = 64;
const REGION_MIN_BYTES: usize = 512 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ShmError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("nix error: {0}")]
    Nix(#[from] nix::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid shared memory header")]
    InvalidHeader,
    #[error("shared memory region already owned by another process")]
    RegionInUse,
    #[error("shared memory slot full")]
    SlotFull,
    #[error("value too long: {0} > {1}")]
    ValueTooLong(usize, usize),
    #[error("invalid name")]
    InvalidName,
}

#[repr(C)]
pub struct ShmHeader {
    pub magic: u32,
    pub version: u32,

    pub router_uuid: [u8; 16],
    pub owner_pid: u32,
    pub _pad0: u32,
    pub owner_start_time: u64,
    pub generation: u64,

    pub seq: AtomicU64,

    pub node_count: u32,
    pub route_count: u32,

    pub node_max: u32,
    pub route_max: u32,

    pub created_at: u64,
    pub updated_at: u64,
    pub heartbeat: u64,

    pub island_id: [u8; 64],
    pub island_id_len: u16,

    pub _reserved: [u8; 30],
}

#[derive(Clone, Copy, Debug)]
pub struct ShmLayout {
    pub header_offset: usize,
    pub nodes_offset: usize,
    pub routes_offset: usize,
    pub total_len: usize,
    pub node_capacity: usize,
    pub route_capacity: usize,
}

impl ShmLayout {
    pub fn new(node_capacity: usize, route_capacity: usize) -> Self {
        let header_size = size_of::<ShmHeader>();
        let nodes_size = node_capacity * size_of::<NodeEntry>();
        let routes_size = route_capacity * size_of::<RouteEntry>();

        let header_offset = 0;
        let nodes_offset = align_up(header_offset + header_size, REGION_ALIGNMENT);
        let routes_offset = align_up(nodes_offset + nodes_size, REGION_ALIGNMENT);
        let mut total_len = align_up(routes_offset + routes_size, REGION_ALIGNMENT);

        if total_len < REGION_MIN_BYTES {
            total_len = REGION_MIN_BYTES;
        }

        Self {
            header_offset,
            nodes_offset,
            routes_offset,
            total_len,
            node_capacity,
            route_capacity,
        }
    }
}

pub struct ShmWriter {
    name: String,
    fd: OwnedFd,
    mmap: MmapMut,
    layout: ShmLayout,
    router_uuid: [u8; 16],
}

pub struct ShmReader {
    name: String,
    fd: OwnedFd,
    mmap: Mmap,
    layout: ShmLayout,
}

#[derive(Clone, Debug)]
pub struct ShmSnapshot {
    pub router_uuid: [u8; 16],
    pub nodes: Vec<NodeEntry>,
    pub routes: Vec<RouteEntry>,
    pub heartbeat: u64,
    pub generation: u64,
}

impl ShmWriter {
    pub fn open_or_create(router_uuid: Uuid, island_id: &str) -> Result<Self, ShmError> {
        let name = shm_name_for_uuid(router_uuid);
        if name.len() > SHM_NAME_LIMIT {
            return Err(ShmError::ValueTooLong(name.len(), SHM_NAME_MAX_LEN));
        }
        let layout = ShmLayout::new(MAX_NODES as usize, MAX_ROUTES as usize);

        match shm_open_with_flags(&name, OFlag::O_RDWR) {
            Ok(fd) => {
                let stat = fstat(&fd)?;
                if stat.st_size < layout.total_len as i64 {
                    shm_unlink_best_effort(&name)?;
                    return recreate_region(&name, layout, router_uuid, island_id, None);
                }

                let mut mmap =
                    unsafe { MmapOptions::new().len(layout.total_len).map_mut(fd.as_raw_fd())? };
                let header = header_mut(&mut mmap)?;
                if !header_valid(header) {
                    drop(mmap);
                    shm_unlink_best_effort(&name)?;
                    return recreate_region(&name, layout, router_uuid, island_id, None);
                }

                if owner_alive(header) && !heartbeat_stale(header) {
                    drop(mmap);
                    return Err(ShmError::RegionInUse);
                }

                let generation = Some(header.generation.saturating_add(1));
                drop(mmap);
                shm_unlink_best_effort(&name)?;
                recreate_region(&name, layout, router_uuid, island_id, generation)
            }
            Err(ShmError::Nix(Errno::ENOENT)) => {
                recreate_region(&name, layout, router_uuid, island_id, None)
            }
            Err(err) => Err(err),
        }
    }

    pub fn header(&self) -> &ShmHeader {
        unsafe { &*(self.mmap.as_ptr() as *const ShmHeader) }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn header_mut(&mut self) -> &mut ShmHeader {
        unsafe { &mut *(self.mmap.as_mut_ptr() as *mut ShmHeader) }
    }

    pub fn nodes_mut(&mut self) -> &mut [NodeEntry] {
        let ptr = unsafe { self.mmap.as_mut_ptr().add(self.layout.nodes_offset) };
        unsafe { std::slice::from_raw_parts_mut(ptr as *mut NodeEntry, self.layout.node_capacity) }
    }

    pub fn routes_mut(&mut self) -> &mut [RouteEntry] {
        let ptr = unsafe { self.mmap.as_mut_ptr().add(self.layout.routes_offset) };
        unsafe { std::slice::from_raw_parts_mut(ptr as *mut RouteEntry, self.layout.route_capacity) }
    }

    fn nodes_slice(&self) -> &[NodeEntry] {
        let ptr = unsafe { self.mmap.as_ptr().add(self.layout.nodes_offset) };
        unsafe { std::slice::from_raw_parts(ptr as *const NodeEntry, self.layout.node_capacity) }
    }

    fn routes_slice(&self) -> &[RouteEntry] {
        let ptr = unsafe { self.mmap.as_ptr().add(self.layout.routes_offset) };
        unsafe { std::slice::from_raw_parts(ptr as *const RouteEntry, self.layout.route_capacity) }
    }

    pub fn read_snapshot(&self) -> Option<ShmSnapshot> {
        loop {
            let header = self.header();
            let s1 = header.seq.load(Ordering::Acquire);
            if s1 & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }

            atomic::fence(Ordering::Acquire);
            let nodes = self.nodes_slice().to_vec();
            let routes = self.routes_slice().to_vec();
            atomic::fence(Ordering::Acquire);

            let s2 = header.seq.load(Ordering::Acquire);
            if s1 == s2 {
                return Some(ShmSnapshot {
                    router_uuid: header.router_uuid,
                    nodes,
                    routes,
                    heartbeat: header.heartbeat,
                    generation: header.generation,
                });
            }
        }
    }

    pub fn begin_write(&self) {
        self.header().seq.fetch_add(1, Ordering::Relaxed);
    }

    pub fn end_write(&self) {
        atomic::fence(Ordering::Release);
        self.header().seq.fetch_add(1, Ordering::Relaxed);
    }

    pub fn register_node(&mut self, node_uuid: Uuid, name: &str) -> Result<(), ShmError> {
        validate_name(name)?;
        let name_len = name.as_bytes().len();
        if name_len > NAME_MAX_LEN {
            return Err(ShmError::ValueTooLong(name_len, NAME_MAX_LEN));
        }

        let now = now_epoch_ms();
        let slot = {
            let nodes = self.nodes_mut();
            find_free_slot(nodes).ok_or(ShmError::SlotFull)?
        };

        self.begin_write();
        {
            let nodes = self.nodes_mut();
            let entry = &mut nodes[slot];
            entry.uuid = *node_uuid.as_bytes();
            entry.name.fill(0);
            entry.name[..name_len].copy_from_slice(name.as_bytes());
            entry.name_len = name_len as u16;
            entry.flags = FLAG_ACTIVE;
            entry.connected_at = now;
        }

        let header = self.header_mut();
        header.node_count = header.node_count.saturating_add(1);
        header.updated_at = now;

        let _ = self.add_connected_route(name, now);
        self.end_write();
        Ok(())
    }

    pub fn unregister_node(&mut self, node_uuid: Uuid) -> Result<(), ShmError> {
        let idx = self
            .nodes_slice()
            .iter()
            .position(|n| n.is_active() && n.uuid == *node_uuid.as_bytes());
        let Some(idx) = idx else {
            return Ok(());
        };

        let (name, name_len) = {
            let node = self.nodes_slice()[idx];
            (node.name, node.name_len)
        };

        let now = now_epoch_ms();
        self.begin_write();
        {
            let nodes = self.nodes_mut();
            let entry = &mut nodes[idx];
            entry.flags = FLAG_DELETED;
        }

        let header = self.header_mut();
        header.node_count = header.node_count.saturating_sub(1);
        header.updated_at = now;

        let _ = self.remove_connected_route_by_name(&name, name_len);
        self.end_write();
        Ok(())
    }

    pub fn update_heartbeat(&mut self) {
        self.begin_write();
        let header = self.header_mut();
        header.heartbeat = now_epoch_ms();
        self.end_write();
    }

    fn add_connected_route(&mut self, prefix: &str, installed_at: u64) -> Result<(), ShmError> {
        let prefix_len = prefix.as_bytes().len();
        if prefix_len > NAME_MAX_LEN {
            return Err(ShmError::ValueTooLong(prefix_len, NAME_MAX_LEN));
        }
        let router_uuid = self.router_uuid;
        let slot = {
            let routes = self.routes_mut();
            find_free_slot(routes).ok_or(ShmError::SlotFull)?
        };

        let routes = self.routes_mut();
        let entry = &mut routes[slot];
        entry.prefix.fill(0);
        entry.prefix[..prefix_len].copy_from_slice(prefix.as_bytes());
        entry.prefix_len = prefix_len as u16;
        entry.match_kind = MATCH_EXACT;
        entry.route_type = ROUTE_CONNECTED;
        entry.next_hop_router = router_uuid;
        entry.out_link = LINK_LOCAL;
        entry.metric = 1;
        entry.admin_distance = AD_CONNECTED;
        entry.flags = FLAG_ACTIVE;
        entry.installed_at = installed_at;

        let header = self.header_mut();
        header.route_count = header.route_count.saturating_add(1);
        Ok(())
    }

    pub fn add_static_route(
        &mut self,
        prefix: &str,
        match_kind: u8,
        next_hop_router: [u8; 16],
        out_link: u32,
        metric: u32,
    ) -> Result<(), ShmError> {
        let prefix_len = prefix.as_bytes().len();
        if prefix_len > NAME_MAX_LEN {
            return Err(ShmError::ValueTooLong(prefix_len, NAME_MAX_LEN));
        }
        let slot = {
            let routes = self.routes_mut();
            find_free_slot(routes).ok_or(ShmError::SlotFull)?
        };
        let routes = self.routes_mut();
        let entry = &mut routes[slot];
        entry.prefix.fill(0);
        entry.prefix[..prefix_len].copy_from_slice(prefix.as_bytes());
        entry.prefix_len = prefix_len as u16;
        entry.match_kind = match_kind;
        entry.route_type = ROUTE_STATIC;
        entry.next_hop_router = next_hop_router;
        entry.out_link = out_link;
        entry.metric = metric;
        entry.admin_distance = AD_STATIC;
        entry.flags = FLAG_ACTIVE;
        entry.installed_at = now_epoch_ms();

        let header = self.header_mut();
        header.route_count = header.route_count.saturating_add(1);
        Ok(())
    }

    fn remove_connected_route_by_name(
        &mut self,
        name: &[u8; 256],
        name_len: u16,
    ) -> Result<(), ShmError> {
        let routes = self.routes_mut();
        let idx = routes.iter().position(|route| {
            route.is_active()
                && route.route_type == ROUTE_CONNECTED
                && route.prefix_len == name_len
                && route.prefix[..name_len as usize] == name[..name_len as usize]
        });
        if let Some(idx) = idx {
            routes[idx].flags = FLAG_DELETED;
            let header = self.header_mut();
            header.route_count = header.route_count.saturating_sub(1);
        }
        Ok(())
    }
}

impl ShmReader {
    pub fn open_read_only(name: &str) -> Result<Self, ShmError> {
        let layout = ShmLayout::new(MAX_NODES as usize, MAX_ROUTES as usize);
        let fd = shm_open_with_flags(name, OFlag::O_RDONLY)?;
        let stat = fstat(&fd)?;
        if stat.st_size < layout.total_len as i64 {
            return Err(ShmError::InvalidHeader);
        }
        let mmap = unsafe { MmapOptions::new().len(layout.total_len).map(fd.as_raw_fd())? };
        let header = header_ref(&mmap)?;
        if !header_valid(header) {
            return Err(ShmError::InvalidHeader);
        }

        Ok(Self {
            name: name.to_string(),
            fd,
            mmap,
            layout,
        })
    }

    pub fn header(&self) -> &ShmHeader {
        unsafe { &*(self.mmap.as_ptr() as *const ShmHeader) }
    }

    pub fn read_snapshot(&self) -> Option<ShmSnapshot> {
        loop {
            let header = self.header();
            let s1 = header.seq.load(Ordering::Acquire);
            if s1 & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }

            atomic::fence(Ordering::Acquire);
            let nodes = self.nodes_slice().to_vec();
            let routes = self.routes_slice().to_vec();
            atomic::fence(Ordering::Acquire);

            let s2 = header.seq.load(Ordering::Acquire);
            if s1 == s2 {
                return Some(ShmSnapshot {
                    router_uuid: header.router_uuid,
                    nodes,
                    routes,
                    heartbeat: header.heartbeat,
                    generation: header.generation,
                });
            }
        }
    }

    fn nodes_slice(&self) -> &[NodeEntry] {
        let ptr = unsafe { self.mmap.as_ptr().add(self.layout.nodes_offset) };
        unsafe { std::slice::from_raw_parts(ptr as *const NodeEntry, self.layout.node_capacity) }
    }

    fn routes_slice(&self) -> &[RouteEntry] {
        let ptr = unsafe { self.mmap.as_ptr().add(self.layout.routes_offset) };
        unsafe { std::slice::from_raw_parts(ptr as *const RouteEntry, self.layout.route_capacity) }
    }
}

fn shm_open_existing(name: &str) -> Result<OwnedFd, ShmError> {
    shm_open_with_flags(name, OFlag::O_RDWR)
}

fn shm_open_with_flags(name: &str, flags: OFlag) -> Result<OwnedFd, ShmError> {
    let cname = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name"))?;
    Ok(shm_open(cname.as_c_str(), flags, Mode::empty())?)
}

fn recreate_region(
    name: &str,
    layout: ShmLayout,
    router_uuid: Uuid,
    island_id: &str,
    generation: Option<u64>,
) -> Result<ShmWriter, ShmError> {
    let cname = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name"))?;
    let fd = shm_open(
        cname.as_c_str(),
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR,
        Mode::from_bits_truncate(0o600),
    )?;
    ftruncate(&fd, layout.total_len as i64)?;
    let mut mmap = unsafe { MmapOptions::new().len(layout.total_len).map_mut(fd.as_raw_fd())? };

    let header = header_mut(&mut mmap)?;
    initialize_header(header, router_uuid, island_id, generation);

    Ok(ShmWriter {
        name: name.to_string(),
        fd,
        mmap,
        layout,
        router_uuid: *router_uuid.as_bytes(),
    })
}

fn initialize_header(header: &mut ShmHeader, router_uuid: Uuid, island_id: &str, generation: Option<u64>) {
    let now = now_epoch_ms();
    header.magic = SHM_MAGIC;
    header.version = SHM_VERSION;
    header.router_uuid = *router_uuid.as_bytes();
    header.owner_pid = std::process::id();
    header._pad0 = 0;
    header.owner_start_time = get_process_start_time_ms(std::process::id()).unwrap_or(0);
    header.generation = generation.unwrap_or(1);
    header.seq.store(0, Ordering::Relaxed);
    header.node_count = 0;
    header.route_count = 0;
    header.node_max = MAX_NODES;
    header.route_max = MAX_ROUTES;
    header.created_at = now;
    header.updated_at = now;
    header.heartbeat = now;

    header.island_id.fill(0);
    let island_bytes = island_id.as_bytes();
    let len = island_bytes.len().min(ISLAND_ID_MAX_LEN);
    header.island_id[..len].copy_from_slice(&island_bytes[..len]);
    header.island_id_len = len as u16;
}

fn header_valid(header: &ShmHeader) -> bool {
    header.magic == SHM_MAGIC
        && header.version == SHM_VERSION
        && header.node_max == MAX_NODES
        && header.route_max == MAX_ROUTES
        && align_of::<ShmHeader>() <= REGION_ALIGNMENT
}

fn heartbeat_stale(header: &ShmHeader) -> bool {
    let now = now_epoch_ms();
    now.saturating_sub(header.heartbeat) > HEARTBEAT_STALE_MS
}

fn owner_alive(header: &ShmHeader) -> bool {
    if header.owner_pid == 0 {
        return false;
    }

    let pid = header.owner_pid as i32;
    if unsafe { nix::libc::kill(pid, 0) } != 0 {
        return false;
    }

    let actual_start = get_process_start_time_ms(header.owner_pid);
    match (header.owner_start_time, actual_start) {
        (0, _) => true,
        (_, None) => true,
        (expected, Some(actual)) => expected == actual,
    }
}

fn shm_unlink_best_effort(name: &str) -> Result<(), ShmError> {
    let cname = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name"))?;
    match nix::sys::mman::shm_unlink(cname.as_c_str()) {
        Ok(_) => Ok(()),
        Err(Errno::ENOENT) => Ok(()),
        Err(err) => Err(ShmError::Nix(err)),
    }
}

fn header_mut(mmap: &mut MmapMut) -> Result<&mut ShmHeader, ShmError> {
    if mmap.len() < size_of::<ShmHeader>() {
        return Err(ShmError::InvalidHeader);
    }
    Ok(unsafe { &mut *(mmap.as_mut_ptr() as *mut ShmHeader) })
}

fn header_ref(mmap: &Mmap) -> Result<&ShmHeader, ShmError> {
    if mmap.len() < size_of::<ShmHeader>() {
        return Err(ShmError::InvalidHeader);
    }
    Ok(unsafe { &*(mmap.as_ptr() as *const ShmHeader) })
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

fn shm_name_for_uuid(router_uuid: Uuid) -> String {
    let full = format!("{SHM_NAME_PREFIX}{router_uuid}");
    if full.len() <= SHM_NAME_LIMIT {
        return full;
    }

    let simple = router_uuid.simple().to_string();
    let short = format!(
        "{}{}{}",
        SHM_NAME_PREFIX,
        &simple[..8],
        &simple[simple.len() - 8..]
    );
    short
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn validate_name(name: &str) -> Result<(), ShmError> {
    if name.is_empty() {
        return Err(ShmError::InvalidName);
    }
    if !name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err(ShmError::InvalidName);
    }
    Ok(())
}

fn find_free_slot<T: HasFlags>(entries: &[T]) -> Option<usize> {
    entries.iter().position(|entry| {
        let flags = entry.flags();
        flags == 0 || (flags & FLAG_DELETED) != 0
    })
}

pub trait HasFlags {
    fn flags(&self) -> u16;
}

#[cfg(target_os = "linux")]
fn get_process_start_time_ms(pid: u32) -> Option<u64> {
    let stat_path = format!("/proc/{}/stat", pid);
    let stat = fs::read_to_string(stat_path).ok()?;
    let end = stat.rfind(')')?;
    let after = stat.get(end + 2..)?;
    let fields: Vec<&str> = after.split_whitespace().collect();
    let start_ticks: u64 = fields.get(19)?.parse().ok()?;

    let hz = unsafe { nix::libc::sysconf(nix::libc::_SC_CLK_TCK) };
    if hz <= 0 {
        return None;
    }
    let hz = hz as u64;

    let btime = read_boot_time_secs()?;
    let start_ms = (btime * 1000).saturating_add(start_ticks.saturating_mul(1000) / hz);
    Some(start_ms)
}

#[cfg(not(target_os = "linux"))]
fn get_process_start_time_ms(_pid: u32) -> Option<u64> {
    None
}

#[cfg(target_os = "linux")]
fn read_boot_time_secs() -> Option<u64> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    for line in stat.lines() {
        if let Some(rest) = line.strip_prefix("btime ") {
            return rest.trim().parse::<u64>().ok();
        }
    }
    None
}
