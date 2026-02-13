use std::ffi::{CStr, CString};
use std::mem::size_of;
use std::os::fd::AsRawFd;
use std::sync::atomic::{self, AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use memmap2::{Mmap, MmapMut, MmapOptions};
use nix::fcntl::OFlag;
use nix::sys::mman::{shm_open, shm_unlink};
use nix::sys::stat::fstat;
use nix::unistd::ftruncate;
use uuid::Uuid;

pub const SHM_MAGIC: u32 = 0x4A535352; // "JSSR"
pub const SHM_VERSION: u32 = 2;

pub const CONFIG_MAGIC: u32 = 0x4A534343; // "JSCC"
pub const CONFIG_VERSION: u32 = 1;

pub const LSA_MAGIC: u32 = 0x4A534C41; // "JSLA"
pub const LSA_VERSION: u32 = 1;

pub const OPA_MAGIC: u32 = 0x4A534F50; // "JSOP"
pub const OPA_VERSION: u32 = 1;
pub const OPA_MAX_WASM_SIZE: usize = 4 * 1024 * 1024;

pub const OPA_STATUS_OK: u8 = 0;
pub const OPA_STATUS_ERROR: u8 = 1;
pub const OPA_STATUS_LOADING: u8 = 2;

const SEQLOCK_READ_TIMEOUT_MS: u64 = 5;

pub const MAX_NODES: u32 = 1024;
pub const MAX_STATIC_ROUTES: u32 = 256;
pub const MAX_VPN_ASSIGNMENTS: u32 = 256;
pub const MAX_REMOTE_HIVES: u32 = 16;
pub const MAX_REMOTE_NODES: u32 = 1024;
pub const MAX_REMOTE_ROUTES: u32 = 256;
pub const MAX_REMOTE_VPNS: u32 = 256;

pub const MATCH_EXACT: u8 = 0;
pub const MATCH_PREFIX: u8 = 1;
pub const MATCH_GLOB: u8 = 2;

pub const ACTION_FORWARD: u8 = 0;
pub const ACTION_DROP: u8 = 1;

pub const FLAG_ACTIVE: u16 = 0x0001;
pub const FLAG_DELETED: u16 = 0x0002;
pub const FLAG_STALE: u16 = 0x0004;

pub const HEARTBEAT_INTERVAL_MS: u64 = 5_000;
pub const HEARTBEAT_STALE_MS: u64 = 30_000;

pub const SHM_NAME_MAX_LEN: usize = 64;
pub const SHM_NAME_LIMIT: usize = SHM_NAME_MAX_LEN;

const REGION_ALIGNMENT: usize = 64;

#[derive(Debug, thiserror::Error)]
pub enum ShmError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("nix error: {0}")]
    Nix(#[from] nix::Error),
    #[error("invalid header")]
    InvalidHeader,
    #[error("name too long")]
    NameTooLong,
    #[error("value too long: {len} > {max}")]
    ValueTooLong { len: usize, max: usize },
    #[error("shared memory slot full")]
    SlotFull,
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
    pub node_max: u32,

    pub created_at: u64,
    pub updated_at: u64,
    pub heartbeat: u64,

    pub hive_id: [u8; 64],
    pub hive_id_len: u16,

    pub router_name: [u8; 64],
    pub router_name_len: u16,

    pub is_gateway: u8,
    pub _flags_reserved: [u8; 3],

    pub opa_policy_version: u64,
    pub opa_load_status: u8,
    pub _opa_pad: [u8; 7],

    pub _reserved: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NodeEntry {
    pub uuid: [u8; 16],
    pub name: [u8; 256],
    pub name_len: u16,
    pub vpn_id: u32,
    pub flags: u16,
    pub connected_at: u64,
    pub _reserved: [u8; 32],
}

#[repr(C)]
pub struct ConfigHeader {
    pub magic: u32,
    pub version: u32,

    pub owner_uuid: [u8; 16],
    pub owner_pid: u32,
    pub _pad0: u32,
    pub owner_start_time: u64,
    pub heartbeat: u64,

    pub seq: AtomicU64,

    pub static_route_count: u32,
    pub vpn_assignment_count: u32,
    pub config_version: u64,

    pub hive_id: [u8; 64],
    pub hive_id_len: u16,

    pub created_at: u64,
    pub updated_at: u64,

    pub _reserved: [u8; 38],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StaticRouteEntry {
    pub prefix: [u8; 256],
    pub prefix_len: u16,
    pub match_kind: u8,
    pub action: u8,

    pub next_hop_hive: [u8; 32],
    pub next_hop_hive_len: u8,
    pub _pad: [u8; 3],

    pub metric: u32,
    pub priority: u16,
    pub flags: u16,
    pub installed_at: u64,
    pub _reserved: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VpnAssignment {
    pub pattern: [u8; 256],
    pub pattern_len: u16,
    pub match_kind: u8,
    pub _pad0: u8,

    pub vpn_id: u32,
    pub priority: u16,
    pub flags: u16,

    pub _reserved: [u8; 20],
}

#[repr(C)]
pub struct LsaHeader {
    pub magic: u32,
    pub version: u32,

    pub gateway_uuid: [u8; 16],
    pub owner_pid: u32,
    pub _pad0: u32,
    pub owner_start_time: u64,
    pub heartbeat: u64,

    pub seq: AtomicU64,

    pub hive_count: u32,
    pub total_node_count: u32,
    pub total_route_count: u32,
    pub total_vpn_count: u32,

    pub created_at: u64,
    pub updated_at: u64,

    pub local_hive_id: [u8; 64],
    pub local_hive_id_len: u16,

    pub _reserved: [u8; 38],
}

#[repr(C)]
pub struct OpaHeader {
    pub magic: u32,
    pub version: u32,

    pub seq: AtomicU64,

    pub policy_version: u64,
    pub wasm_size: u32,
    pub _pad0: u32,
    pub wasm_hash: [u8; 32],

    pub updated_at: u64,
    pub heartbeat: u64,

    pub status: u8,
    pub _pad1: u8,

    pub entrypoint: [u8; 64],
    pub entrypoint_len: u16,

    pub owner_uuid: [u8; 16],
    pub owner_pid: u32,
    pub _pad2: u32,

    pub _reserved: [u8; 20],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RemoteHiveEntry {
    pub hive_id: [u8; 64],
    pub hive_id_len: u16,

    pub last_lsa_seq: u64,
    pub last_updated: u64,
    pub flags: u16,

    pub node_count: u32,
    pub route_count: u32,
    pub vpn_count: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RemoteNodeEntry {
    pub uuid: [u8; 16],
    pub name: [u8; 256],
    pub name_len: u16,

    pub vpn_id: u32,
    pub hive_index: u16,

    pub flags: u16,
    pub _reserved: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RemoteRouteEntry {
    pub prefix: [u8; 256],
    pub prefix_len: u16,
    pub match_kind: u8,
    pub action: u8,

    pub next_hop_hive: [u8; 32],
    pub next_hop_hive_len: u8,
    pub _pad: [u8; 3],

    pub metric: u32,
    pub priority: u16,
    pub flags: u16,
    pub hive_index: u16,
    pub _reserved: [u8; 14],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RemoteVpnEntry {
    pub pattern: [u8; 256],
    pub pattern_len: u16,
    pub match_kind: u8,
    pub _pad0: u8,

    pub vpn_id: u32,
    pub priority: u16,
    pub flags: u16,

    pub hive_index: u16,
    pub _reserved: [u8; 18],
}

#[derive(Clone, Copy)]
struct RegionLayout {
    header_offset: usize,
    node_offset: usize,
    static_offset: usize,
    vpn_offset: usize,
    hive_offset: usize,
    remote_node_offset: usize,
    remote_route_offset: usize,
    remote_vpn_offset: usize,
    total_len: usize,
}

#[derive(Debug)]
pub struct ShmSnapshot {
    pub header: ShmHeaderSnapshot,
    pub nodes: Vec<NodeEntry>,
}

#[derive(Debug, Clone)]
pub struct ShmHeaderSnapshot {
    pub router_uuid: Uuid,
    pub router_name: String,
    pub hive_id: String,
    pub is_gateway: bool,
    pub node_count: u32,
    pub heartbeat: u64,
    pub generation: u64,
    pub opa_policy_version: u64,
    pub opa_load_status: u8,
}

#[derive(Debug)]
pub struct ConfigSnapshot {
    pub header: ConfigHeaderSnapshot,
    pub routes: Vec<StaticRouteEntry>,
    pub vpns: Vec<VpnAssignment>,
}

#[derive(Debug, Clone, Copy)]
pub struct ConfigHeaderSnapshot {
    pub static_route_count: u32,
    pub vpn_assignment_count: u32,
    pub config_version: u64,
    pub heartbeat: u64,
}

#[derive(Debug, Clone)]
pub struct LsaSnapshot {
    pub header: LsaHeaderSnapshot,
    pub hives: Vec<RemoteHiveEntry>,
    pub nodes: Vec<RemoteNodeEntry>,
    pub routes: Vec<RemoteRouteEntry>,
    pub vpns: Vec<RemoteVpnEntry>,
}

#[derive(Debug, Clone, Copy)]
pub struct LsaHeaderSnapshot {
    pub hive_count: u32,
    pub total_node_count: u32,
    pub total_route_count: u32,
    pub total_vpn_count: u32,
    pub heartbeat: u64,
}

#[derive(Debug, Clone)]
pub struct OpaSnapshot {
    pub header: OpaHeaderSnapshot,
    pub wasm: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct OpaHeaderSnapshot {
    pub policy_version: u64,
    pub wasm_size: u32,
    pub wasm_hash: [u8; 32],
    pub updated_at: u64,
    pub heartbeat: u64,
    pub status: u8,
    pub entrypoint: String,
    pub owner_uuid: Uuid,
    pub owner_pid: u32,
}

pub struct RouterRegionWriter {
    name: String,
    mmap: MmapMut,
    layout: RegionLayout,
}

pub struct RouterRegionReader {
    name: String,
    mmap: Mmap,
    layout: RegionLayout,
}

pub struct ConfigRegionWriter {
    name: String,
    mmap: MmapMut,
    layout: RegionLayout,
}

pub struct ConfigRegionReader {
    name: String,
    mmap: Mmap,
    layout: RegionLayout,
}

pub struct LsaRegionWriter {
    name: String,
    mmap: MmapMut,
    layout: RegionLayout,
}

pub struct LsaRegionReader {
    name: String,
    mmap: Mmap,
    layout: RegionLayout,
}

pub struct OpaRegionReader {
    name: String,
    mmap: Mmap,
}

impl RouterRegionWriter {
    pub fn open_or_create(
        name: &str,
        router_uuid: Uuid,
        hive_id: &str,
        router_name: &str,
        is_gateway: bool,
    ) -> Result<Self, ShmError> {
        validate_name(name)?;
        let layout = layout_router();
        let mmap = open_or_create_region(name, layout.total_len, |mmap| {
            initialize_router_header(mmap, router_uuid, hive_id, router_name, is_gateway);
        })?;
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<ShmSnapshot> {
        let header = self.header_ref()?;
        read_router_snapshot(header, self.mmap.as_ref(), &self.layout)
    }

    pub fn update_heartbeat(&mut self) {
        if let Some(header) = self.header_mut() {
            seqlock_begin_write(&header.seq);
            header.heartbeat = now_epoch_ms();
            seqlock_end_write(&header.seq);
        }
    }

    pub fn update_opa_status(&mut self, policy_version: u64, load_status: u8) {
        let Some(header) = self.header_mut() else {
            return;
        };
        if header.opa_policy_version == policy_version && header.opa_load_status == load_status {
            return;
        }
        seqlock_begin_write(&header.seq);
        header.opa_policy_version = policy_version;
        header.opa_load_status = load_status;
        header.updated_at = now_epoch_ms();
        header.heartbeat = header.updated_at;
        seqlock_end_write(&header.seq);
    }

    pub fn register_node(
        &mut self,
        node_uuid: Uuid,
        name: &str,
        vpn_id: u32,
        connected_at: u64,
    ) -> Result<(), ShmError> {
        if name.as_bytes().len() > 256 {
            return Err(ShmError::ValueTooLong {
                len: name.len(),
                max: 256,
            });
        }
        let (header, nodes): (&mut ShmHeader, &mut [NodeEntry]) =
            router_header_and_nodes_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;
        let mut slot = None;
        for entry in nodes.iter_mut() {
            if entry.flags & FLAG_ACTIVE == 0 {
                slot = Some(entry);
                break;
            }
            if entry.uuid == *node_uuid.as_bytes() {
                slot = Some(entry);
                break;
            }
        }
        let Some(entry) = slot else {
            return Err(ShmError::SlotFull);
        };

        seqlock_begin_write(&header.seq);
        *entry = empty_node_entry();
        entry.uuid = *node_uuid.as_bytes();
        entry.name_len = copy_bytes_with_len(&mut entry.name, name) as u16;
        entry.vpn_id = vpn_id;
        entry.flags = FLAG_ACTIVE;
        entry.connected_at = connected_at;
        header.node_count = count_active_nodes(nodes);
        header.updated_at = now_epoch_ms();
        header.heartbeat = header.updated_at;
        seqlock_end_write(&header.seq);
        Ok(())
    }

    pub fn unregister_node(&mut self, node_uuid: Uuid) -> Result<(), ShmError> {
        let (header, nodes): (&mut ShmHeader, &mut [NodeEntry]) =
            router_header_and_nodes_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;
        let mut found = false;
        seqlock_begin_write(&header.seq);
        for entry in nodes.iter_mut() {
            if entry.flags & FLAG_ACTIVE == 0 {
                continue;
            }
            if entry.uuid == *node_uuid.as_bytes() {
                *entry = empty_node_entry();
                entry.flags = FLAG_DELETED;
                found = true;
                break;
            }
        }
        if found {
            header.node_count = count_active_nodes(nodes);
            header.updated_at = now_epoch_ms();
            header.heartbeat = header.updated_at;
        }
        seqlock_end_write(&header.seq);
        Ok(())
    }

    pub fn update_node_vpn(&mut self, node_uuid: Uuid, vpn_id: u32) -> Result<(), ShmError> {
        let (header, nodes): (&mut ShmHeader, &mut [NodeEntry]) =
            router_header_and_nodes_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;
        let mut updated = false;
        seqlock_begin_write(&header.seq);
        for entry in nodes.iter_mut() {
            if entry.flags & FLAG_ACTIVE == 0 {
                continue;
            }
            if entry.uuid == *node_uuid.as_bytes() {
                entry.vpn_id = vpn_id;
                updated = true;
                break;
            }
        }
        if updated {
            header.updated_at = now_epoch_ms();
            header.heartbeat = header.updated_at;
        }
        seqlock_end_write(&header.seq);
        Ok(())
    }

    fn header_ref(&self) -> Option<&ShmHeader> {
        header_ref::<ShmHeader>(self.mmap.as_ref(), self.layout.header_offset)
    }

    fn header_mut(&mut self) -> Option<&mut ShmHeader> {
        header_mut::<ShmHeader>(&mut self.mmap, self.layout.header_offset)
    }

    fn nodes_mut(&mut self) -> Option<&mut [NodeEntry]> {
        let offset = self.layout.node_offset;
        slice_mut::<NodeEntry>(&mut self.mmap, offset, MAX_NODES as usize)
    }
}

impl RouterRegionReader {
    pub fn open_read_only(name: &str) -> Result<Self, ShmError> {
        validate_name(name)?;
        let layout = layout_router();
        let mmap = open_read_only_region(name, layout.total_len)?;
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<ShmSnapshot> {
        let header = self.header_ref()?;
        read_router_snapshot(header, self.mmap.as_ref(), &self.layout)
    }

    fn header_ref(&self) -> Option<&ShmHeader> {
        header_ref::<ShmHeader>(self.mmap.as_ref(), self.layout.header_offset)
    }
}

impl ConfigRegionWriter {
    pub fn open_or_create(name: &str, owner_uuid: Uuid, hive_id: &str) -> Result<Self, ShmError> {
        validate_name(name)?;
        let layout = layout_config();
        let mmap = open_or_create_region(name, layout.total_len, |mmap| {
            initialize_config_header(mmap, owner_uuid, hive_id);
        })?;
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<ConfigSnapshot> {
        let header = self.header_ref()?;
        read_config_snapshot(header, self.mmap.as_ref(), &self.layout)
    }

    pub fn update_heartbeat(&mut self) {
        if let Some(header) = self.header_mut() {
            seqlock_begin_write(&header.seq);
            header.heartbeat = now_epoch_ms();
            seqlock_end_write(&header.seq);
        }
    }

    pub fn write_static_routes(
        &mut self,
        routes: &[StaticRouteEntry],
        bump_version: bool,
    ) -> Result<(), ShmError> {
        if routes.len() > MAX_STATIC_ROUTES as usize {
            return Err(ShmError::ValueTooLong {
                len: routes.len(),
                max: MAX_STATIC_ROUTES as usize,
            });
        }
        let (header, entries): (&mut ConfigHeader, &mut [StaticRouteEntry]) =
            config_header_and_routes_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;
        seqlock_begin_write(&header.seq);
        for entry in entries.iter_mut() {
            *entry = empty_static_route();
        }
        for (idx, route) in routes.iter().enumerate() {
            entries[idx] = *route;
        }
        header.static_route_count = routes.len() as u32;
        if bump_version {
            header.config_version = header.config_version.saturating_add(1);
        }
        header.updated_at = now_epoch_ms();
        header.heartbeat = header.updated_at;
        seqlock_end_write(&header.seq);
        Ok(())
    }

    pub fn write_vpn_assignments(
        &mut self,
        vpns: &[VpnAssignment],
        bump_version: bool,
    ) -> Result<(), ShmError> {
        if vpns.len() > MAX_VPN_ASSIGNMENTS as usize {
            return Err(ShmError::ValueTooLong {
                len: vpns.len(),
                max: MAX_VPN_ASSIGNMENTS as usize,
            });
        }
        let (header, entries): (&mut ConfigHeader, &mut [VpnAssignment]) =
            config_header_and_vpns_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;
        seqlock_begin_write(&header.seq);
        for entry in entries.iter_mut() {
            *entry = empty_vpn_assignment();
        }
        for (idx, vpn) in vpns.iter().enumerate() {
            entries[idx] = *vpn;
        }
        header.vpn_assignment_count = vpns.len() as u32;
        if bump_version {
            header.config_version = header.config_version.saturating_add(1);
        }
        header.updated_at = now_epoch_ms();
        header.heartbeat = header.updated_at;
        seqlock_end_write(&header.seq);
        Ok(())
    }

    fn header_ref(&self) -> Option<&ConfigHeader> {
        header_ref::<ConfigHeader>(self.mmap.as_ref(), self.layout.header_offset)
    }

    fn header_mut(&mut self) -> Option<&mut ConfigHeader> {
        header_mut::<ConfigHeader>(&mut self.mmap, self.layout.header_offset)
    }

    fn routes_mut(&mut self) -> Option<&mut [StaticRouteEntry]> {
        let offset = self.layout.static_offset;
        slice_mut::<StaticRouteEntry>(&mut self.mmap, offset, MAX_STATIC_ROUTES as usize)
    }

    fn vpns_mut(&mut self) -> Option<&mut [VpnAssignment]> {
        let offset = self.layout.vpn_offset;
        slice_mut::<VpnAssignment>(&mut self.mmap, offset, MAX_VPN_ASSIGNMENTS as usize)
    }
}

impl ConfigRegionReader {
    pub fn open_read_only(name: &str) -> Result<Self, ShmError> {
        validate_name(name)?;
        let layout = layout_config();
        let mmap = open_read_only_region(name, layout.total_len)?;
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<ConfigSnapshot> {
        let header = self.header_ref()?;
        read_config_snapshot(header, self.mmap.as_ref(), &self.layout)
    }

    fn header_ref(&self) -> Option<&ConfigHeader> {
        header_ref::<ConfigHeader>(self.mmap.as_ref(), self.layout.header_offset)
    }
}

impl LsaRegionWriter {
    pub fn open_or_create(
        name: &str,
        gateway_uuid: Uuid,
        hive_id: &str,
    ) -> Result<Self, ShmError> {
        validate_name(name)?;
        let layout = layout_lsa();
        let mmap = open_or_create_region(name, layout.total_len, |mmap| {
            initialize_lsa_header(mmap, gateway_uuid, hive_id);
        })?;
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<LsaSnapshot> {
        let header = self.header_ref()?;
        read_lsa_snapshot(header, self.mmap.as_ref(), &self.layout)
    }

    pub fn update_heartbeat(&mut self) {
        if let Some(header) = self.header_mut() {
            seqlock_begin_write(&header.seq);
            header.heartbeat = now_epoch_ms();
            seqlock_end_write(&header.seq);
        }
    }

    pub fn write_snapshot(
        &mut self,
        hives: &[RemoteHiveEntry],
        nodes: &[RemoteNodeEntry],
        routes: &[RemoteRouteEntry],
        vpns: &[RemoteVpnEntry],
    ) -> Result<(), ShmError> {
        if hives.len() > MAX_REMOTE_HIVES as usize {
            return Err(ShmError::ValueTooLong {
                len: hives.len(),
                max: MAX_REMOTE_HIVES as usize,
            });
        }
        if nodes.len() > MAX_REMOTE_NODES as usize {
            return Err(ShmError::ValueTooLong {
                len: nodes.len(),
                max: MAX_REMOTE_NODES as usize,
            });
        }
        if routes.len() > MAX_REMOTE_ROUTES as usize {
            return Err(ShmError::ValueTooLong {
                len: routes.len(),
                max: MAX_REMOTE_ROUTES as usize,
            });
        }
        if vpns.len() > MAX_REMOTE_VPNS as usize {
            return Err(ShmError::ValueTooLong {
                len: vpns.len(),
                max: MAX_REMOTE_VPNS as usize,
            });
        }
        let (header, hive_entries, node_entries, route_entries, vpn_entries): (
            &mut LsaHeader,
            &mut [RemoteHiveEntry],
            &mut [RemoteNodeEntry],
            &mut [RemoteRouteEntry],
            &mut [RemoteVpnEntry],
        ) = lsa_header_and_entries_mut(&mut self.mmap, &self.layout)
            .ok_or(ShmError::InvalidHeader)?;

        seqlock_begin_write(&header.seq);
        for entry in hive_entries.iter_mut() {
            *entry = empty_remote_hive();
        }
        for entry in node_entries.iter_mut() {
            *entry = empty_remote_node();
        }
        for entry in route_entries.iter_mut() {
            *entry = empty_remote_route();
        }
        for entry in vpn_entries.iter_mut() {
            *entry = empty_remote_vpn();
        }

        for (idx, hive) in hives.iter().enumerate() {
            hive_entries[idx] = *hive;
        }
        for (idx, node) in nodes.iter().enumerate() {
            node_entries[idx] = *node;
        }
        for (idx, route) in routes.iter().enumerate() {
            route_entries[idx] = *route;
        }
        for (idx, vpn) in vpns.iter().enumerate() {
            vpn_entries[idx] = *vpn;
        }

        header.hive_count = hives.len() as u32;
        header.total_node_count = nodes.len() as u32;
        header.total_route_count = routes.len() as u32;
        header.total_vpn_count = vpns.len() as u32;
        header.updated_at = now_epoch_ms();
        header.heartbeat = header.updated_at;
        seqlock_end_write(&header.seq);
        Ok(())
    }

    fn header_ref(&self) -> Option<&LsaHeader> {
        header_ref::<LsaHeader>(self.mmap.as_ref(), self.layout.header_offset)
    }

    fn header_mut(&mut self) -> Option<&mut LsaHeader> {
        header_mut::<LsaHeader>(&mut self.mmap, self.layout.header_offset)
    }

    fn hives_mut(&mut self) -> Option<&mut [RemoteHiveEntry]> {
        let offset = self.layout.hive_offset;
        slice_mut::<RemoteHiveEntry>(&mut self.mmap, offset, MAX_REMOTE_HIVES as usize)
    }

    fn remote_nodes_mut(&mut self) -> Option<&mut [RemoteNodeEntry]> {
        let offset = self.layout.remote_node_offset;
        slice_mut::<RemoteNodeEntry>(&mut self.mmap, offset, MAX_REMOTE_NODES as usize)
    }

    fn remote_routes_mut(&mut self) -> Option<&mut [RemoteRouteEntry]> {
        let offset = self.layout.remote_route_offset;
        slice_mut::<RemoteRouteEntry>(&mut self.mmap, offset, MAX_REMOTE_ROUTES as usize)
    }

    fn remote_vpns_mut(&mut self) -> Option<&mut [RemoteVpnEntry]> {
        let offset = self.layout.remote_vpn_offset;
        slice_mut::<RemoteVpnEntry>(&mut self.mmap, offset, MAX_REMOTE_VPNS as usize)
    }
}

impl LsaRegionReader {
    pub fn open_read_only(name: &str) -> Result<Self, ShmError> {
        validate_name(name)?;
        let layout = layout_lsa();
        let mmap = open_read_only_region(name, layout.total_len)?;
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<LsaSnapshot> {
        let header = self.header_ref()?;
        read_lsa_snapshot(header, self.mmap.as_ref(), &self.layout)
    }

    fn header_ref(&self) -> Option<&LsaHeader> {
        header_ref::<LsaHeader>(self.mmap.as_ref(), self.layout.header_offset)
    }
}

impl OpaRegionReader {
    pub fn open_read_only(name: &str) -> Result<Self, ShmError> {
        validate_name(name)?;
        let mmap = open_read_only_opa_region(name)?;
        Ok(Self {
            name: name.to_string(),
            mmap,
        })
    }

    pub fn read_snapshot(&self) -> Option<OpaSnapshot> {
        let header = self.header_ref()?;
        read_opa_snapshot(header, self.mmap.as_ref())
    }

    pub fn read_header(&self) -> Option<OpaHeaderSnapshot> {
        let header = self.header_ref()?;
        read_opa_header_snapshot(header)
    }

    fn header_ref(&self) -> Option<&OpaHeader> {
        header_ref::<OpaHeader>(self.mmap.as_ref(), 0)
    }
}

pub fn build_shm_name(prefix: &str, router_uuid: Uuid) -> String {
    let simple = router_uuid.simple().to_string();
    let name = format!("{}{}", prefix, simple);
    if name.len() <= SHM_NAME_LIMIT {
        return name;
    }
    format!("{}{}", prefix, &simple[..16])
}

pub fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn seqlock_begin_write(seq: &AtomicU64) {
    seq.fetch_add(1, Ordering::Relaxed);
}

pub fn seqlock_end_write(seq: &AtomicU64) {
    atomic::fence(Ordering::Release);
    seq.fetch_add(1, Ordering::Relaxed);
}

pub fn copy_bytes_with_len(dst: &mut [u8], src: &str) -> usize {
    let bytes = src.as_bytes();
    let len = bytes.len().min(dst.len());
    dst[..len].copy_from_slice(&bytes[..len]);
    len
}

fn layout_router() -> RegionLayout {
    let header_size = size_of::<ShmHeader>();
    let node_size = size_of::<NodeEntry>() * MAX_NODES as usize;

    let header_offset = 0;
    let node_offset = align_up(header_offset + header_size, REGION_ALIGNMENT);
    let total_len = align_up(node_offset + node_size, REGION_ALIGNMENT);

    RegionLayout {
        header_offset,
        node_offset,
        static_offset: 0,
        vpn_offset: 0,
        hive_offset: 0,
        remote_node_offset: 0,
        remote_route_offset: 0,
        remote_vpn_offset: 0,
        total_len,
    }
}

fn layout_config() -> RegionLayout {
    let header_size = size_of::<ConfigHeader>();
    let routes_size = size_of::<StaticRouteEntry>() * MAX_STATIC_ROUTES as usize;
    let vpns_size = size_of::<VpnAssignment>() * MAX_VPN_ASSIGNMENTS as usize;

    let header_offset = 0;
    let static_offset = align_up(header_offset + header_size, REGION_ALIGNMENT);
    let vpn_offset = align_up(static_offset + routes_size, REGION_ALIGNMENT);
    let total_len = align_up(vpn_offset + vpns_size, REGION_ALIGNMENT);

    RegionLayout {
        header_offset,
        node_offset: 0,
        static_offset,
        vpn_offset,
        hive_offset: 0,
        remote_node_offset: 0,
        remote_route_offset: 0,
        remote_vpn_offset: 0,
        total_len,
    }
}

fn layout_lsa() -> RegionLayout {
    let header_size = size_of::<LsaHeader>();
    let hive_size = size_of::<RemoteHiveEntry>() * MAX_REMOTE_HIVES as usize;
    let node_size = size_of::<RemoteNodeEntry>() * MAX_REMOTE_NODES as usize;
    let route_size = size_of::<RemoteRouteEntry>() * MAX_REMOTE_ROUTES as usize;
    let vpn_size = size_of::<RemoteVpnEntry>() * MAX_REMOTE_VPNS as usize;

    let header_offset = 0;
    let hive_offset = align_up(header_offset + header_size, REGION_ALIGNMENT);
    let remote_node_offset = align_up(hive_offset + hive_size, REGION_ALIGNMENT);
    let remote_route_offset = align_up(remote_node_offset + node_size, REGION_ALIGNMENT);
    let remote_vpn_offset = align_up(remote_route_offset + route_size, REGION_ALIGNMENT);
    let total_len = align_up(remote_vpn_offset + vpn_size, REGION_ALIGNMENT);

    RegionLayout {
        header_offset,
        node_offset: 0,
        static_offset: 0,
        vpn_offset: 0,
        hive_offset,
        remote_node_offset,
        remote_route_offset,
        remote_vpn_offset,
        total_len,
    }
}

fn opa_region_len() -> usize {
    size_of::<OpaHeader>() + OPA_MAX_WASM_SIZE
}

fn open_or_create_region<F>(name: &str, len: usize, init: F) -> Result<MmapMut, ShmError>
where
    F: FnOnce(&mut MmapMut),
{
    let cstr = CString::new(name).map_err(|_| ShmError::NameTooLong)?;
    let name_cstr: &CStr = cstr.as_c_str();
    loop {
        match shm_open(name_cstr, OFlag::O_RDWR, nix::sys::stat::Mode::empty()) {
            Ok(fd) => {
                let stat = fstat(fd.as_raw_fd())?;
                if stat.st_size < len as i64 {
                    let _ = shm_unlink(name_cstr);
                } else {
                    let mmap = unsafe { MmapOptions::new().len(len).map_mut(fd.as_raw_fd())? };
                    if is_region_valid(mmap.as_ref()) {
                        return Ok(mmap);
                    }
                    let _ = shm_unlink(name_cstr);
                }
            }
            Err(err) => {
                if err != nix::Error::ENOENT {
                    return Err(err.into());
                }
            }
        }

        match shm_open(
            name_cstr,
            OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_RDWR,
            nix::sys::stat::Mode::from_bits_truncate(0o600),
        ) {
            Ok(fd) => {
                ftruncate(&fd, len as i64)?;
                let mut mmap = unsafe { MmapOptions::new().len(len).map_mut(fd.as_raw_fd())? };
                for byte in mmap.iter_mut() {
                    *byte = 0;
                }
                init(&mut mmap);
                return Ok(mmap);
            }
            Err(err) => {
                if err == nix::Error::EEXIST {
                    continue;
                }
                return Err(err.into());
            }
        }
    }
}

fn open_read_only_region(name: &str, len: usize) -> Result<Mmap, ShmError> {
    let cstr = CString::new(name).map_err(|_| ShmError::NameTooLong)?;
    let fd = shm_open(
        cstr.as_c_str(),
        OFlag::O_RDONLY,
        nix::sys::stat::Mode::empty(),
    )?;
    let mmap = unsafe { MmapOptions::new().len(len).map(fd.as_raw_fd())? };
    if !is_region_valid(mmap.as_ref()) {
        return Err(ShmError::InvalidHeader);
    }
    Ok(mmap)
}

fn open_read_only_opa_region(name: &str) -> Result<Mmap, ShmError> {
    let cstr = CString::new(name).map_err(|_| ShmError::NameTooLong)?;
    let fd = shm_open(
        cstr.as_c_str(),
        OFlag::O_RDONLY,
        nix::sys::stat::Mode::empty(),
    )?;
    let mmap = unsafe {
        MmapOptions::new()
            .len(opa_region_len())
            .map(fd.as_raw_fd())?
    };
    if !is_opa_region_valid(mmap.as_ref()) {
        return Err(ShmError::InvalidHeader);
    }
    Ok(mmap)
}

fn is_region_valid(mmap: &[u8]) -> bool {
    let Some(magic_bytes) = mmap.get(0..4) else {
        return false;
    };
    let Ok(magic_bytes) = <[u8; 4]>::try_from(magic_bytes) else {
        return false;
    };
    let magic = u32::from_ne_bytes(magic_bytes);
    match magic {
        SHM_MAGIC => {
            header_ref::<ShmHeader>(mmap, 0).is_some_and(|header| header.version == SHM_VERSION)
        }
        CONFIG_MAGIC => header_ref::<ConfigHeader>(mmap, 0)
            .is_some_and(|header| header.version == CONFIG_VERSION),
        LSA_MAGIC => {
            header_ref::<LsaHeader>(mmap, 0).is_some_and(|header| header.version == LSA_VERSION)
        }
        _ => false,
    }
}

fn is_opa_region_valid(mmap: &[u8]) -> bool {
    if mmap.len() < size_of::<OpaHeader>() {
        return false;
    }
    let Some(magic_bytes) = mmap.get(0..4) else {
        return false;
    };
    let Ok(magic_bytes) = <[u8; 4]>::try_from(magic_bytes) else {
        return false;
    };
    let magic = u32::from_ne_bytes(magic_bytes);
    if magic != OPA_MAGIC {
        return false;
    }
    header_ref::<OpaHeader>(mmap, 0).is_some_and(|header| header.version == OPA_VERSION)
}

fn initialize_router_header(
    mmap: &mut MmapMut,
    router_uuid: Uuid,
    hive_id: &str,
    router_name: &str,
    is_gateway: bool,
) {
    if let Some(header) = header_mut::<ShmHeader>(mmap, 0) {
        header.magic = SHM_MAGIC;
        header.version = SHM_VERSION;
        header.router_uuid = *router_uuid.as_bytes();
        header.owner_pid = std::process::id();
        header.owner_start_time = now_epoch_ms();
        header.generation = 1;
        header.seq = AtomicU64::new(0);
        header.node_count = 0;
        header.node_max = MAX_NODES;
        header.created_at = now_epoch_ms();
        header.updated_at = header.created_at;
        header.heartbeat = header.created_at;
        header.hive_id_len = copy_bytes_with_len(&mut header.hive_id, hive_id) as u16;
        header.router_name_len = copy_bytes_with_len(&mut header.router_name, router_name) as u16;
        header.is_gateway = if is_gateway { 1 } else { 0 };
        header.opa_policy_version = 0;
        header.opa_load_status = 0;
    }
}

fn initialize_config_header(mmap: &mut MmapMut, owner_uuid: Uuid, hive_id: &str) {
    if let Some(header) = header_mut::<ConfigHeader>(mmap, 0) {
        header.magic = CONFIG_MAGIC;
        header.version = CONFIG_VERSION;
        header.owner_uuid = *owner_uuid.as_bytes();
        header.owner_pid = std::process::id();
        header.owner_start_time = now_epoch_ms();
        header.heartbeat = now_epoch_ms();
        header.seq = AtomicU64::new(0);
        header.static_route_count = 0;
        header.vpn_assignment_count = 0;
        header.config_version = 1;
        header.hive_id_len = copy_bytes_with_len(&mut header.hive_id, hive_id) as u16;
        header.created_at = now_epoch_ms();
        header.updated_at = header.created_at;
    }
}

fn initialize_lsa_header(mmap: &mut MmapMut, gateway_uuid: Uuid, hive_id: &str) {
    if let Some(header) = header_mut::<LsaHeader>(mmap, 0) {
        header.magic = LSA_MAGIC;
        header.version = LSA_VERSION;
        header.gateway_uuid = *gateway_uuid.as_bytes();
        header.owner_pid = std::process::id();
        header.owner_start_time = now_epoch_ms();
        header.heartbeat = now_epoch_ms();
        header.seq = AtomicU64::new(0);
        header.hive_count = 0;
        header.total_node_count = 0;
        header.total_route_count = 0;
        header.total_vpn_count = 0;
        header.local_hive_id_len =
            copy_bytes_with_len(&mut header.local_hive_id, hive_id) as u16;
        header.created_at = now_epoch_ms();
        header.updated_at = header.created_at;
    }
}

fn read_router_snapshot(
    header: &ShmHeader,
    mmap: &[u8],
    layout: &RegionLayout,
) -> Option<ShmSnapshot> {
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            return None;
        }
        let s1 = header.seq.load(Ordering::Acquire);
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);
        let node_count = header.node_count as usize;
        let nodes = read_slice::<NodeEntry>(mmap, layout.node_offset, MAX_NODES as usize)?;
        let mut snapshot = Vec::new();
        for node in nodes.iter().take(node_count) {
            snapshot.push(*node);
        }
        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            let router_uuid = Uuid::from_bytes(header.router_uuid);
            let router_name = read_string(&header.router_name, header.router_name_len as usize);
            let hive_id = read_string(&header.hive_id, header.hive_id_len as usize);
            return Some(ShmSnapshot {
                header: ShmHeaderSnapshot {
                    router_uuid,
                    router_name,
                    hive_id,
                    is_gateway: header.is_gateway != 0,
                    node_count: header.node_count,
                    heartbeat: header.heartbeat,
                    generation: header.generation,
                    opa_policy_version: header.opa_policy_version,
                    opa_load_status: header.opa_load_status,
                },
                nodes: snapshot,
            });
        }
    }
}

fn read_string(buf: &[u8], len: usize) -> String {
    let len = len.min(buf.len());
    std::str::from_utf8(&buf[..len]).unwrap_or("").to_string()
}

fn read_config_snapshot(
    header: &ConfigHeader,
    mmap: &[u8],
    layout: &RegionLayout,
) -> Option<ConfigSnapshot> {
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            return None;
        }
        let s1 = header.seq.load(Ordering::Acquire);
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);
        let route_count = header.static_route_count as usize;
        let vpn_count = header.vpn_assignment_count as usize;
        let routes =
            read_slice::<StaticRouteEntry>(mmap, layout.static_offset, MAX_STATIC_ROUTES as usize)?;
        let vpns =
            read_slice::<VpnAssignment>(mmap, layout.vpn_offset, MAX_VPN_ASSIGNMENTS as usize)?;
        let mut route_snapshot = Vec::new();
        for route in routes.iter().take(route_count) {
            route_snapshot.push(*route);
        }
        let mut vpn_snapshot = Vec::new();
        for vpn in vpns.iter().take(vpn_count) {
            vpn_snapshot.push(*vpn);
        }
        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Some(ConfigSnapshot {
                header: ConfigHeaderSnapshot {
                    static_route_count: header.static_route_count,
                    vpn_assignment_count: header.vpn_assignment_count,
                    config_version: header.config_version,
                    heartbeat: header.heartbeat,
                },
                routes: route_snapshot,
                vpns: vpn_snapshot,
            });
        }
    }
}

fn read_lsa_snapshot(
    header: &LsaHeader,
    mmap: &[u8],
    layout: &RegionLayout,
) -> Option<LsaSnapshot> {
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            return None;
        }
        let s1 = header.seq.load(Ordering::Acquire);
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);
        let hive_count = header.hive_count as usize;
        let total_nodes = header.total_node_count as usize;
        let total_routes = header.total_route_count as usize;
        let total_vpns = header.total_vpn_count as usize;

        let hives = read_slice::<RemoteHiveEntry>(
            mmap,
            layout.hive_offset,
            MAX_REMOTE_HIVES as usize,
        )?;
        let nodes = read_slice::<RemoteNodeEntry>(
            mmap,
            layout.remote_node_offset,
            MAX_REMOTE_NODES as usize,
        )?;
        let routes = read_slice::<RemoteRouteEntry>(
            mmap,
            layout.remote_route_offset,
            MAX_REMOTE_ROUTES as usize,
        )?;
        let vpns =
            read_slice::<RemoteVpnEntry>(mmap, layout.remote_vpn_offset, MAX_REMOTE_VPNS as usize)?;

        let mut hive_snapshot = Vec::new();
        for hive in hives.iter().take(hive_count) {
            hive_snapshot.push(*hive);
        }
        let mut node_snapshot = Vec::new();
        for node in nodes.iter().take(total_nodes) {
            node_snapshot.push(*node);
        }
        let mut route_snapshot = Vec::new();
        for route in routes.iter().take(total_routes) {
            route_snapshot.push(*route);
        }
        let mut vpn_snapshot = Vec::new();
        for vpn in vpns.iter().take(total_vpns) {
            vpn_snapshot.push(*vpn);
        }
        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Some(LsaSnapshot {
                header: LsaHeaderSnapshot {
                    hive_count: header.hive_count,
                    total_node_count: header.total_node_count,
                    total_route_count: header.total_route_count,
                    total_vpn_count: header.total_vpn_count,
                    heartbeat: header.heartbeat,
                },
                hives: hive_snapshot,
                nodes: node_snapshot,
                routes: route_snapshot,
                vpns: vpn_snapshot,
            });
        }
    }
}

fn read_opa_header_snapshot(header: &OpaHeader) -> Option<OpaHeaderSnapshot> {
    let start = Instant::now();
    loop {
        if start.elapsed() > Duration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            return None;
        }
        let s1 = header.seq.load(Ordering::Acquire);
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);
        let entrypoint = read_string(&header.entrypoint, header.entrypoint_len as usize);
        let snapshot = OpaHeaderSnapshot {
            policy_version: header.policy_version,
            wasm_size: header.wasm_size,
            wasm_hash: header.wasm_hash,
            updated_at: header.updated_at,
            heartbeat: header.heartbeat,
            status: header.status,
            entrypoint,
            owner_uuid: Uuid::from_bytes(header.owner_uuid),
            owner_pid: header.owner_pid,
        };
        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Some(snapshot);
        }
    }
}

fn read_opa_snapshot(header: &OpaHeader, mmap: &[u8]) -> Option<OpaSnapshot> {
    let start = Instant::now();
    let header_size = size_of::<OpaHeader>();
    loop {
        if start.elapsed() > Duration::from_millis(SEQLOCK_READ_TIMEOUT_MS) {
            return None;
        }
        let s1 = header.seq.load(Ordering::Acquire);
        if s1 & 1 != 0 {
            std::hint::spin_loop();
            continue;
        }
        atomic::fence(Ordering::Acquire);
        let wasm_size = header.wasm_size as usize;
        if wasm_size > OPA_MAX_WASM_SIZE {
            return None;
        }
        let entrypoint = read_string(&header.entrypoint, header.entrypoint_len as usize);
        let header_snapshot = OpaHeaderSnapshot {
            policy_version: header.policy_version,
            wasm_size: header.wasm_size,
            wasm_hash: header.wasm_hash,
            updated_at: header.updated_at,
            heartbeat: header.heartbeat,
            status: header.status,
            entrypoint,
            owner_uuid: Uuid::from_bytes(header.owner_uuid),
            owner_pid: header.owner_pid,
        };
        let wasm = if wasm_size == 0 {
            Vec::new()
        } else {
            let data = mmap.get(header_size..header_size + wasm_size)?;
            data.to_vec()
        };
        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Some(OpaSnapshot {
                header: header_snapshot,
                wasm,
            });
        }
    }
}

fn read_slice<T: Copy>(mmap: &[u8], offset: usize, len: usize) -> Option<&[T]> {
    let bytes = len.checked_mul(size_of::<T>())?;
    let ptr = mmap.as_ptr().wrapping_add(offset) as *const T;
    if offset + bytes > mmap.len() {
        return None;
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn slice_mut<T: Copy>(mmap: &mut MmapMut, offset: usize, len: usize) -> Option<&mut [T]> {
    let bytes = len.checked_mul(size_of::<T>())?;
    if offset + bytes > mmap.len() {
        return None;
    }
    let ptr = mmap.as_mut_ptr().wrapping_add(offset) as *mut T;
    Some(unsafe { std::slice::from_raw_parts_mut(ptr, len) })
}

fn header_ref<T>(mmap: &[u8], offset: usize) -> Option<&T> {
    if offset + size_of::<T>() > mmap.len() {
        return None;
    }
    let ptr = mmap.as_ptr().wrapping_add(offset) as *const T;
    Some(unsafe { &*ptr })
}

fn header_mut<T>(mmap: &mut MmapMut, offset: usize) -> Option<&mut T> {
    if offset + size_of::<T>() > mmap.len() {
        return None;
    }
    let ptr = mmap.as_mut_ptr().wrapping_add(offset) as *mut T;
    Some(unsafe { &mut *ptr })
}

fn router_header_and_nodes_mut<'a>(
    mmap: &'a mut MmapMut,
    layout: &RegionLayout,
) -> Option<(&'a mut ShmHeader, &'a mut [NodeEntry])> {
    let header_offset = layout.header_offset;
    let nodes_offset = layout.node_offset;
    let nodes_len = MAX_NODES as usize;
    header_and_slice_mut::<ShmHeader, NodeEntry>(mmap, header_offset, nodes_offset, nodes_len)
}

fn config_header_and_routes_mut<'a>(
    mmap: &'a mut MmapMut,
    layout: &RegionLayout,
) -> Option<(&'a mut ConfigHeader, &'a mut [StaticRouteEntry])> {
    let header_offset = layout.header_offset;
    let routes_offset = layout.static_offset;
    let routes_len = MAX_STATIC_ROUTES as usize;
    header_and_slice_mut::<ConfigHeader, StaticRouteEntry>(
        mmap,
        header_offset,
        routes_offset,
        routes_len,
    )
}

fn config_header_and_vpns_mut<'a>(
    mmap: &'a mut MmapMut,
    layout: &RegionLayout,
) -> Option<(&'a mut ConfigHeader, &'a mut [VpnAssignment])> {
    let header_offset = layout.header_offset;
    let vpns_offset = layout.vpn_offset;
    let vpns_len = MAX_VPN_ASSIGNMENTS as usize;
    header_and_slice_mut::<ConfigHeader, VpnAssignment>(mmap, header_offset, vpns_offset, vpns_len)
}

fn lsa_header_and_entries_mut<'a>(
    mmap: &'a mut MmapMut,
    layout: &RegionLayout,
) -> Option<(
    &'a mut LsaHeader,
    &'a mut [RemoteHiveEntry],
    &'a mut [RemoteNodeEntry],
    &'a mut [RemoteRouteEntry],
    &'a mut [RemoteVpnEntry],
)> {
    let header_offset = layout.header_offset;
    let hives_offset = layout.hive_offset;
    let nodes_offset = layout.remote_node_offset;
    let routes_offset = layout.remote_route_offset;
    let vpns_offset = layout.remote_vpn_offset;
    let hives_len = MAX_REMOTE_HIVES as usize;
    let nodes_len = MAX_REMOTE_NODES as usize;
    let routes_len = MAX_REMOTE_ROUTES as usize;
    let vpns_len = MAX_REMOTE_VPNS as usize;
    let len = mmap.len();
    let base = mmap.as_mut_ptr();
    if header_offset + size_of::<LsaHeader>() > len {
        return None;
    }
    if vpns_offset + size_of::<RemoteVpnEntry>() * vpns_len > len {
        return None;
    }
    unsafe {
        let header_ptr = base.add(header_offset) as *mut LsaHeader;
        let hives_ptr = base.add(hives_offset) as *mut RemoteHiveEntry;
        let nodes_ptr = base.add(nodes_offset) as *mut RemoteNodeEntry;
        let routes_ptr = base.add(routes_offset) as *mut RemoteRouteEntry;
        let vpns_ptr = base.add(vpns_offset) as *mut RemoteVpnEntry;
        Some((
            &mut *header_ptr,
            std::slice::from_raw_parts_mut(hives_ptr, hives_len),
            std::slice::from_raw_parts_mut(nodes_ptr, nodes_len),
            std::slice::from_raw_parts_mut(routes_ptr, routes_len),
            std::slice::from_raw_parts_mut(vpns_ptr, vpns_len),
        ))
    }
}

fn header_and_slice_mut<T, U>(
    mmap: &mut MmapMut,
    header_offset: usize,
    slice_offset: usize,
    slice_len: usize,
) -> Option<(&mut T, &mut [U])> {
    let header_size = size_of::<T>();
    let slice_size = size_of::<U>().checked_mul(slice_len)?;
    if header_offset + header_size > mmap.len() {
        return None;
    }
    if slice_offset + slice_size > mmap.len() {
        return None;
    }
    let base = mmap.as_mut_ptr();
    unsafe {
        let header_ptr = base.add(header_offset) as *mut T;
        let slice_ptr = base.add(slice_offset) as *mut U;
        Some((
            &mut *header_ptr,
            std::slice::from_raw_parts_mut(slice_ptr, slice_len),
        ))
    }
}

fn align_up(value: usize, align: usize) -> usize {
    let mask = align - 1;
    (value + mask) & !mask
}

fn validate_name(name: &str) -> Result<(), ShmError> {
    if name.len() > SHM_NAME_LIMIT {
        return Err(ShmError::NameTooLong);
    }
    Ok(())
}

fn count_active_nodes(nodes: &[NodeEntry]) -> u32 {
    nodes
        .iter()
        .filter(|node| node.flags & FLAG_ACTIVE != 0)
        .count() as u32
}

fn empty_node_entry() -> NodeEntry {
    NodeEntry {
        uuid: [0u8; 16],
        name: [0u8; 256],
        name_len: 0,
        vpn_id: 0,
        flags: 0,
        connected_at: 0,
        _reserved: [0u8; 32],
    }
}

fn empty_static_route() -> StaticRouteEntry {
    StaticRouteEntry {
        prefix: [0u8; 256],
        prefix_len: 0,
        match_kind: 0,
        action: 0,
        next_hop_hive: [0u8; 32],
        next_hop_hive_len: 0,
        _pad: [0u8; 3],
        metric: 0,
        priority: 0,
        flags: 0,
        installed_at: 0,
        _reserved: [0u8; 8],
    }
}

fn empty_vpn_assignment() -> VpnAssignment {
    VpnAssignment {
        pattern: [0u8; 256],
        pattern_len: 0,
        match_kind: 0,
        _pad0: 0,
        vpn_id: 0,
        priority: 0,
        flags: 0,
        _reserved: [0u8; 20],
    }
}

fn empty_remote_hive() -> RemoteHiveEntry {
    RemoteHiveEntry {
        hive_id: [0u8; 64],
        hive_id_len: 0,
        last_lsa_seq: 0,
        last_updated: 0,
        flags: 0,
        node_count: 0,
        route_count: 0,
        vpn_count: 0,
    }
}

fn empty_remote_node() -> RemoteNodeEntry {
    RemoteNodeEntry {
        uuid: [0u8; 16],
        name: [0u8; 256],
        name_len: 0,
        vpn_id: 0,
        hive_index: 0,
        flags: 0,
        _reserved: [0u8; 6],
    }
}

fn empty_remote_route() -> RemoteRouteEntry {
    RemoteRouteEntry {
        prefix: [0u8; 256],
        prefix_len: 0,
        match_kind: 0,
        action: 0,
        next_hop_hive: [0u8; 32],
        next_hop_hive_len: 0,
        _pad: [0u8; 3],
        metric: 0,
        priority: 0,
        flags: 0,
        hive_index: 0,
        _reserved: [0u8; 14],
    }
}

fn empty_remote_vpn() -> RemoteVpnEntry {
    RemoteVpnEntry {
        pattern: [0u8; 256],
        pattern_len: 0,
        match_kind: 0,
        _pad0: 0,
        vpn_id: 0,
        priority: 0,
        flags: 0,
        hive_index: 0,
        _reserved: [0u8; 18],
    }
}
