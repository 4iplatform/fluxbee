use std::ffi::CString;
use std::io;
use std::mem::size_of;
use std::sync::atomic::{self, AtomicU64, Ordering};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use memmap2::{Mmap, MmapMut, MmapOptions};
use nix::fcntl::OFlag;
use nix::sys::stat::fstat;
use nix::sys::stat::Mode;

use crate::shm::SHM_NAME_MAX_LEN;

const CONFIG_MAGIC: u32 = 0x4A534352; // "JSCR"
const CONFIG_VERSION: u32 = 1;
const ISLAND_MAX_LEN: usize = 32;
const NEXT_HOP_MAX_LEN: usize = 64;
const PREFIX_MAX_LEN: usize = 64;
const MAX_LOCAL_ROUTES: usize = 256;
const MAX_GLOBAL_ROUTES: usize = 512;
pub const CONFIG_HEARTBEAT_STALE_MS: u64 = 30_000;

pub const ACTION_FORWARD: u8 = 0;
pub const ACTION_DROP: u8 = 1;
pub const ACTION_VPN: u8 = 2;

#[derive(Debug, thiserror::Error)]
pub enum ConfigShmError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("nix error: {0}")]
    Nix(#[from] nix::Error),
    #[error("invalid header")]
    InvalidHeader,
    #[error("name too long")]
    NameTooLong,
    #[error("value too long")]
    ValueTooLong,
}

#[repr(C)]
#[derive(Debug)]
pub struct ConfigHeader {
    pub magic: u32,
    pub version: u32,
    pub seq: AtomicU64,
    pub owner_pid: u32,
    pub heartbeat: u64,
    pub local_version: u64,
    pub global_version: u64,
    pub local_route_count: u32,
    pub global_route_count: u32,
    pub island_len: u8,
    pub island: [u8; ISLAND_MAX_LEN],
    pub reserved: [u8; 7],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ConfigRouteEntry {
    pub prefix_len: u8,
    pub prefix: [u8; PREFIX_MAX_LEN],
    pub match_kind: u8,
    pub action: u8,
    pub next_hop_island_len: u8,
    pub _pad1: u8,
    pub next_hop_island: [u8; NEXT_HOP_MAX_LEN],
    pub vpn_id: u32,
    pub metric: u16,
    pub priority: u16,
    pub flags: u16,
    pub island_len: u8,
    pub _pad2: u8,
    pub island: [u8; ISLAND_MAX_LEN],
    pub version: u64,
}

#[derive(Clone, Copy)]
pub struct ConfigLayout {
    pub header_offset: usize,
    pub local_offset: usize,
    pub global_offset: usize,
    pub total_len: usize,
}

#[derive(Clone, Debug)]
pub struct LocalRoute {
    pub prefix: String,
    pub match_kind: u8,
    pub action: u8,
    pub next_hop_island: Option<String>,
    pub vpn_id: Option<u32>,
    pub metric: u16,
    pub priority: u16,
}

#[derive(Clone, Debug)]
pub struct GlobalRoute {
    pub island: String,
    pub version: u64,
    pub prefix: String,
    pub match_kind: u8,
    pub action: u8,
    pub metric: u16,
    pub priority: u16,
}

pub struct ConfigShmWriter {
    name: String,
    mmap: MmapMut,
    layout: ConfigLayout,
}

pub struct ConfigShmReader {
    name: String,
    mmap: Mmap,
    layout: ConfigLayout,
}

#[derive(Clone, Copy, Debug)]
pub struct ConfigHeaderSnapshot {
    pub heartbeat: u64,
    pub local_version: u64,
    pub global_version: u64,
    pub local_route_count: u32,
    pub global_route_count: u32,
}

#[derive(Clone, Debug)]
pub struct ConfigSnapshot {
    pub header: ConfigHeaderSnapshot,
    pub local_routes: Vec<ConfigRouteEntry>,
    pub global_routes: Vec<ConfigRouteEntry>,
}

impl ConfigShmWriter {
    pub fn open_or_create(name: &str, island: &str) -> Result<Self, ConfigShmError> {
        if name.len() > SHM_NAME_MAX_LEN {
            return Err(ConfigShmError::NameTooLong);
        }
        let layout = layout_for();
        match shm_open_with_flags(name, OFlag::O_RDWR) {
            Ok(fd) => {
                let mmap = unsafe { MmapOptions::new().len(layout.total_len).map_mut(fd.as_raw_fd())? };
                let writer = Self {
                    name: name.to_string(),
                    mmap,
                    layout,
                };
                if !writer.is_valid() {
                    return recreate_region(name, island, layout);
                }
                Ok(writer)
            }
            Err(ConfigShmError::Io(err)) if err.kind() == io::ErrorKind::NotFound => {
                recreate_region(name, island, layout)
            }
            Err(err) => Err(err.into()),
        }
    }

    pub fn update_heartbeat(&mut self) {
        self.begin_write();
        let header = self.header_mut();
        header.heartbeat = now_epoch_ms();
        self.end_write();
    }

    pub fn write_local_routes(&mut self, routes: &[LocalRoute]) -> Result<(), ConfigShmError> {
        if routes.len() > MAX_LOCAL_ROUTES {
            return Err(ConfigShmError::ValueTooLong);
        }
        self.begin_write();
        let local = unsafe {
            std::slice::from_raw_parts_mut(
                self.mmap.as_mut_ptr().add(self.layout.local_offset) as *mut ConfigRouteEntry,
                MAX_LOCAL_ROUTES,
            )
        };
        for entry in local.iter_mut() {
            *entry = empty_route();
        }
        for (idx, route) in routes.iter().enumerate() {
            local[idx] = build_route(route, None);
        }
        let header = self.header_mut();
        header.local_route_count = routes.len() as u32;
        header.local_version = header.local_version.saturating_add(1);
        header.heartbeat = now_epoch_ms();
        self.end_write();
        Ok(())
    }

    pub fn update_global_routes(
        &mut self,
        island: &str,
        version: u64,
        routes: &[LocalRoute],
    ) -> Result<(), ConfigShmError> {
        if routes.len() > MAX_GLOBAL_ROUTES {
            return Err(ConfigShmError::ValueTooLong);
        }
        self.begin_write();
        let global = unsafe {
            std::slice::from_raw_parts_mut(
                self.mmap.as_mut_ptr().add(self.layout.global_offset) as *mut ConfigRouteEntry,
                MAX_GLOBAL_ROUTES,
            )
        };

        for entry in global.iter_mut() {
            if entry.island_len == 0 {
                continue;
            }
            let existing = std::str::from_utf8(&entry.island[..entry.island_len as usize])
                .unwrap_or("");
            if existing == island {
                *entry = empty_route();
            }
        }

        let mut idx = 0usize;
        for entry in global.iter() {
            if entry.island_len == 0 {
                break;
            }
            idx += 1;
        }
        for route in routes {
            if idx >= MAX_GLOBAL_ROUTES {
                break;
            }
            let mut entry = build_route(route, Some(island));
            entry.version = version;
            global[idx] = entry;
            idx += 1;
        }
        let header = self.header_mut();
        header.global_route_count = global.iter().filter(|r| r.island_len > 0).count() as u32;
        header.global_version = header.global_version.saturating_add(1);
        header.heartbeat = now_epoch_ms();
        self.end_write();
        Ok(())
    }

    fn begin_write(&self) {
        self.header().seq.fetch_add(1, Ordering::Relaxed);
    }

    fn end_write(&self) {
        atomic::fence(Ordering::Release);
        self.header().seq.fetch_add(1, Ordering::Relaxed);
    }

    fn header_mut(&mut self) -> &mut ConfigHeader {
        unsafe { &mut *(self.mmap.as_mut_ptr().add(self.layout.header_offset) as *mut ConfigHeader) }
    }

    fn header(&self) -> &ConfigHeader {
        unsafe { &*(self.mmap.as_ptr().add(self.layout.header_offset) as *const ConfigHeader) }
    }

    fn is_valid(&self) -> bool {
        let header = self.header();
        header.magic == CONFIG_MAGIC && header.version == CONFIG_VERSION
    }
}

impl ConfigShmReader {
    pub fn open_read_only(name: &str) -> Result<Self, ConfigShmError> {
        if name.len() > SHM_NAME_MAX_LEN {
            return Err(ConfigShmError::NameTooLong);
        }
        let layout = layout_for();
        let fd = shm_open_with_flags(name, OFlag::O_RDONLY)?;
        let stat = fstat(&fd)?;
        if stat.st_size < layout.total_len as i64 {
            return Err(ConfigShmError::InvalidHeader);
        }
        let mmap = unsafe { MmapOptions::new().len(layout.total_len).map(fd.as_raw_fd())? };
        let header = unsafe {
            &*(mmap.as_ptr().add(layout.header_offset) as *const ConfigHeader)
        };
        if header.magic != CONFIG_MAGIC || header.version != CONFIG_VERSION {
            return Err(ConfigShmError::InvalidHeader);
        }
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<ConfigSnapshot> {
        loop {
            let header = self.header();
            let s1 = header.seq.load(Ordering::Acquire);
            if s1 & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }

            atomic::fence(Ordering::Acquire);
            let local_count = header.local_route_count as usize;
            let global_count = header.global_route_count as usize;
            let local = self.local_slice().get(..local_count)?.to_vec();
            let global = self.global_slice().get(..global_count)?.to_vec();
            let header_snapshot = ConfigHeaderSnapshot {
                heartbeat: header.heartbeat,
                local_version: header.local_version,
                global_version: header.global_version,
                local_route_count: header.local_route_count,
                global_route_count: header.global_route_count,
            };
            atomic::fence(Ordering::Acquire);
            let s2 = header.seq.load(Ordering::Acquire);
            if s1 == s2 {
                return Some(ConfigSnapshot {
                    header: header_snapshot,
                    local_routes: local,
                    global_routes: global,
                });
            }
        }
    }

    fn header(&self) -> &ConfigHeader {
        unsafe { &*(self.mmap.as_ptr().add(self.layout.header_offset) as *const ConfigHeader) }
    }

    fn local_slice(&self) -> &[ConfigRouteEntry] {
        let ptr = unsafe { self.mmap.as_ptr().add(self.layout.local_offset) };
        unsafe { std::slice::from_raw_parts(ptr as *const ConfigRouteEntry, MAX_LOCAL_ROUTES) }
    }

    fn global_slice(&self) -> &[ConfigRouteEntry] {
        let ptr = unsafe { self.mmap.as_ptr().add(self.layout.global_offset) };
        unsafe { std::slice::from_raw_parts(ptr as *const ConfigRouteEntry, MAX_GLOBAL_ROUTES) }
    }
}

fn layout_for() -> ConfigLayout {
    let header_offset = 0usize;
    let local_offset = align_up(header_offset + size_of::<ConfigHeader>(), 8);
    let global_offset = align_up(
        local_offset + size_of::<ConfigRouteEntry>() * MAX_LOCAL_ROUTES,
        8,
    );
    let total_len = align_up(global_offset + size_of::<ConfigRouteEntry>() * MAX_GLOBAL_ROUTES, 8);
    ConfigLayout {
        header_offset,
        local_offset,
        global_offset,
        total_len,
    }
}

fn recreate_region(
    name: &str,
    island: &str,
    layout: ConfigLayout,
) -> Result<ConfigShmWriter, ConfigShmError> {
    let fd = shm_open_with_flags(
        name,
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR,
    )?;
    ftruncate_fd(&fd, layout.total_len as i64)?;
    let mut mmap = unsafe { MmapOptions::new().len(layout.total_len).map_mut(fd.as_raw_fd())? };
    let header = unsafe { &mut *(mmap.as_mut_ptr() as *mut ConfigHeader) };
    initialize_header(header, island);
    let mut local = unsafe {
        std::slice::from_raw_parts_mut(
            mmap.as_mut_ptr().add(layout.local_offset) as *mut ConfigRouteEntry,
            MAX_LOCAL_ROUTES,
        )
    };
    let mut global = unsafe {
        std::slice::from_raw_parts_mut(
            mmap.as_mut_ptr().add(layout.global_offset) as *mut ConfigRouteEntry,
            MAX_GLOBAL_ROUTES,
        )
    };
    for entry in local.iter_mut() {
        *entry = empty_route();
    }
    for entry in global.iter_mut() {
        *entry = empty_route();
    }
    Ok(ConfigShmWriter {
        name: name.to_string(),
        mmap,
        layout,
    })
}

fn initialize_header(header: &mut ConfigHeader, island: &str) {
    let mut hdr = ConfigHeader {
        magic: CONFIG_MAGIC,
        version: CONFIG_VERSION,
        seq: AtomicU64::new(0),
        owner_pid: std::process::id(),
        heartbeat: now_epoch_ms(),
        local_version: 1,
        global_version: 1,
        local_route_count: 0,
        global_route_count: 0,
        island_len: 0,
        island: [0u8; ISLAND_MAX_LEN],
        reserved: [0u8; 7],
    };
    let island_bytes = island.as_bytes();
    let len = island_bytes.len().min(ISLAND_MAX_LEN);
    hdr.island_len = len as u8;
    hdr.island[..len].copy_from_slice(&island_bytes[..len]);
    *header = hdr;
}

fn build_route(route: &LocalRoute, island: Option<&str>) -> ConfigRouteEntry {
    let mut entry = empty_route();
    let prefix_bytes = route.prefix.as_bytes();
    let len = prefix_bytes.len().min(PREFIX_MAX_LEN);
    entry.prefix_len = len as u8;
    entry.prefix[..len].copy_from_slice(&prefix_bytes[..len]);
    entry.match_kind = route.match_kind;
    entry.action = route.action;
    if let Some(next_hop) = route.next_hop_island.as_ref() {
        let bytes = next_hop.as_bytes();
        let next_len = bytes.len().min(NEXT_HOP_MAX_LEN);
        entry.next_hop_island_len = next_len as u8;
        entry.next_hop_island[..next_len].copy_from_slice(&bytes[..next_len]);
    }
    entry.vpn_id = route.vpn_id.unwrap_or(0);
    entry.metric = route.metric;
    entry.priority = route.priority;
    entry.flags = 1;
    if let Some(island) = island {
        let island_bytes = island.as_bytes();
        let island_len = island_bytes.len().min(ISLAND_MAX_LEN);
        entry.island_len = island_len as u8;
        entry.island[..island_len].copy_from_slice(&island_bytes[..island_len]);
    }
    entry
}

fn empty_route() -> ConfigRouteEntry {
    ConfigRouteEntry {
        prefix_len: 0,
        prefix: [0u8; PREFIX_MAX_LEN],
        match_kind: 0,
        action: 0,
        next_hop_island_len: 0,
        _pad1: 0,
        next_hop_island: [0u8; NEXT_HOP_MAX_LEN],
        vpn_id: 0,
        metric: 0,
        priority: 0,
        flags: 0,
        island_len: 0,
        _pad2: 0,
        island: [0u8; ISLAND_MAX_LEN],
        version: 0,
    }
}

fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

fn ftruncate_fd(fd: &OwnedFd, len: i64) -> Result<(), ConfigShmError> {
    let res = unsafe { nix::libc::ftruncate(fd.as_raw_fd(), len as nix::libc::off_t) };
    if res != 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(())
}

fn shm_open_with_flags(name: &str, flags: OFlag) -> Result<OwnedFd, ConfigShmError> {
    let cname = CString::new(name).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "name"))?;
    let mode = Mode::from_bits_truncate(0o600);
    let fd = unsafe {
        nix::libc::shm_open(
            cname.as_ptr(),
            flags.bits(),
            mode.bits() as nix::libc::c_uint,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    // SAFETY: fd is a valid OS file descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn now_epoch_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
