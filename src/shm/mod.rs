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
pub const LSA_VERSION: u32 = 2;

pub const OPA_MAGIC: u32 = 0x4A534F50; // "JSOP"
pub const OPA_VERSION: u32 = 1;
pub const OPA_MAX_WASM_SIZE: usize = 4 * 1024 * 1024;

pub const IDENTITY_MAGIC: u32 = 0x4A534944; // "JSID"
pub const IDENTITY_VERSION: u32 = 2;

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

pub const DEFAULT_IDENTITY_MAX_ILKS: u32 = 1_000_000;
pub const DEFAULT_IDENTITY_MAX_TENANTS: u32 = 10_000;
pub const DEFAULT_IDENTITY_MAX_VOCABULARY: u32 = 4_096;
pub const DEFAULT_IDENTITY_MAX_ILK_ALIASES: u32 = 1_000_000;
pub const ICH_CHANNEL_TYPE_MAX_LEN: usize = 32;
pub const ICH_ADDRESS_MAX_LEN: usize = 256;

pub const MATCH_EXACT: u8 = 0;
pub const MATCH_PREFIX: u8 = 1;
pub const MATCH_GLOB: u8 = 2;

pub const ACTION_FORWARD: u8 = 0;
pub const ACTION_DROP: u8 = 1;

pub const FLAG_ACTIVE: u16 = 0x0001;
pub const FLAG_DELETED: u16 = 0x0002;
pub const FLAG_STALE: u16 = 0x0004;
pub const ICH_MAP_FLAG_OCCUPIED: u16 = 0x0001;
pub const ICH_MAP_FLAG_TOMBSTONE: u16 = 0x0002;

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
pub struct IdentityHeader {
    pub magic: u32,
    pub version: u32,
    pub seq: AtomicU64,

    pub tenant_count: u32,
    pub ilk_count: u32,
    pub ich_count: u32,
    pub ich_mapping_count: u32,
    pub vocabulary_count: u32,
    pub ilk_alias_count: u32,

    pub max_ilks: u32,
    pub max_tenants: u32,
    pub max_ichs: u32,
    pub max_ich_mappings: u32,
    pub max_vocabulary: u32,
    pub max_ilk_aliases: u32,

    pub updated_at: u64,
    pub heartbeat: u64,

    pub owner_uuid: [u8; 16],
    pub owner_pid: u32,
    pub is_primary: u8,
    pub _pad0: [u8; 3],

    pub hive_id: [u8; 64],
    pub hive_id_len: u16,

    pub _reserved: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TenantEntry {
    pub tenant_id: [u8; 16],
    pub name: [u8; 128],
    pub domain: [u8; 128],
    pub status: u8,
    pub flags: u16,
    pub _pad0: [u8; 5],
    pub max_ilks: u32,
    pub created_at: u64,
    pub updated_at: u64,
    pub _reserved: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IlkEntry {
    pub ilk_id: [u8; 16],
    pub ilk_type: u8,
    pub registration_status: u8,
    pub flags: u16,
    pub tenant_id: [u8; 16],
    pub display_name: [u8; 128],
    pub handler_node: [u8; 128],
    pub ich_offset: u32,
    pub ich_count: u16,
    pub _pad0: [u8; 2],
    pub roles_offset: u32,
    pub roles_len: u16,
    pub _pad1: [u8; 2],
    pub capabilities_offset: u32,
    pub capabilities_len: u16,
    pub _pad2: [u8; 2],
    pub created_at: u64,
    pub updated_at: u64,
    pub _reserved: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IchEntry {
    pub ich_id: [u8; 16],
    pub ilk_id: [u8; 16],
    pub channel_type: [u8; ICH_CHANNEL_TYPE_MAX_LEN],
    pub address: [u8; ICH_ADDRESS_MAX_LEN],
    pub flags: u16,
    pub is_primary: u8,
    pub _pad0: [u8; 5],
    pub added_at: u64,
    pub _reserved: [u8; 16],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IchMappingEntry {
    pub hash: u64,
    pub channel_type: [u8; ICH_CHANNEL_TYPE_MAX_LEN],
    pub address: [u8; ICH_ADDRESS_MAX_LEN],
    pub ich_id: [u8; 16],
    pub ilk_id: [u8; 16],
    pub flags: u16,
    pub _reserved: [u8; 54],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IlkAliasEntry {
    pub old_ilk_id: [u8; 16],
    pub canonical_ilk_id: [u8; 16],
    pub expires_at: u64,
    pub flags: u16,
    pub _reserved: [u8; 22],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VocabularyEntry {
    pub tag: [u8; 64],
    pub tag_len: u16,
    pub category: u8,
    pub flags: u8,
    pub description: [u8; 128],
    pub description_len: u16,
    pub _pad0: [u8; 6],
    pub created_at: u64,
    pub deprecated_at: u64,
    pub _reserved: [u8; 8],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RemoteHiveEntry {
    pub hive_id: [u8; 64],
    pub hive_id_len: u16,
    pub router_uuid: [u8; 16],
    pub router_name: [u8; 64],
    pub router_name_len: u16,

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

#[derive(Clone, Copy, Debug)]
pub struct IdentityRegionLimits {
    pub max_ilks: u32,
    pub max_tenants: u32,
    pub max_vocabulary: u32,
    pub max_ilk_aliases: u32,
}

impl Default for IdentityRegionLimits {
    fn default() -> Self {
        Self {
            max_ilks: DEFAULT_IDENTITY_MAX_ILKS,
            max_tenants: DEFAULT_IDENTITY_MAX_TENANTS,
            max_vocabulary: DEFAULT_IDENTITY_MAX_VOCABULARY,
            max_ilk_aliases: DEFAULT_IDENTITY_MAX_ILK_ALIASES,
        }
    }
}

impl IdentityRegionLimits {
    pub fn max_ichs(self) -> u32 {
        self.max_ilks.saturating_mul(4)
    }

    pub fn max_ich_mappings(self) -> u32 {
        self.max_ichs().saturating_mul(2)
    }
}

#[derive(Clone, Copy)]
struct IdentityRegionLayout {
    header_offset: usize,
    tenant_offset: usize,
    ilk_offset: usize,
    ich_offset: usize,
    ich_mapping_offset: usize,
    ilk_alias_offset: usize,
    vocabulary_offset: usize,
    variable_offset: usize,
    total_len: usize,
    limits: IdentityRegionLimits,
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
pub struct IdentitySnapshot {
    pub header: IdentityHeaderSnapshot,
    pub tenants: Vec<TenantEntry>,
    pub ilks: Vec<IlkEntry>,
    pub ichs: Vec<IchEntry>,
    pub ich_mappings: Vec<IchMappingEntry>,
    pub ilk_aliases: Vec<IlkAliasEntry>,
    pub vocabulary: Vec<VocabularyEntry>,
}

#[derive(Debug, Clone, Copy)]
pub struct IdentityHeaderSnapshot {
    pub tenant_count: u32,
    pub ilk_count: u32,
    pub ich_count: u32,
    pub ich_mapping_count: u32,
    pub vocabulary_count: u32,
    pub ilk_alias_count: u32,
    pub max_ilks: u32,
    pub max_tenants: u32,
    pub max_ichs: u32,
    pub max_ich_mappings: u32,
    pub max_vocabulary: u32,
    pub max_ilk_aliases: u32,
    pub heartbeat: u64,
    pub updated_at: u64,
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

pub struct IdentityRegionWriter {
    name: String,
    mmap: MmapMut,
    layout: IdentityRegionLayout,
}

pub struct IdentityRegionReader {
    name: String,
    mmap: Mmap,
    layout: IdentityRegionLayout,
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
    pub fn open_or_create(name: &str, gateway_uuid: Uuid, hive_id: &str) -> Result<Self, ShmError> {
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

impl IdentityRegionWriter {
    pub fn open_or_create(
        name: &str,
        owner_uuid: Uuid,
        hive_id: &str,
        is_primary: bool,
        limits: IdentityRegionLimits,
    ) -> Result<Self, ShmError> {
        validate_name(name)?;
        let layout = layout_identity(limits);
        let mmap = open_or_create_region(name, layout.total_len, |mmap| {
            initialize_identity_header(mmap, owner_uuid, hive_id, is_primary, limits);
        })?;
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<IdentitySnapshot> {
        let header = self.header_ref()?;
        read_identity_snapshot(header, self.mmap.as_ref(), &self.layout)
    }

    pub fn update_heartbeat(&mut self) {
        if let Some(header) = self.header_mut() {
            seqlock_begin_write(&header.seq);
            header.heartbeat = now_epoch_ms();
            seqlock_end_write(&header.seq);
        }
    }

    pub fn clear(&mut self) -> Result<(), ShmError> {
        let (header, tenants, ilks, ichs, mappings, aliases, vocabulary) =
            identity_header_and_entries_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;
        seqlock_begin_write(&header.seq);
        for entry in tenants.iter_mut() {
            *entry = empty_tenant_entry();
        }
        for entry in ilks.iter_mut() {
            *entry = empty_ilk_entry();
        }
        for entry in ichs.iter_mut() {
            *entry = empty_ich_entry();
        }
        for entry in mappings.iter_mut() {
            *entry = empty_ich_mapping_entry();
        }
        for entry in aliases.iter_mut() {
            *entry = empty_ilk_alias_entry();
        }
        for entry in vocabulary.iter_mut() {
            *entry = empty_vocabulary_entry();
        }
        header.tenant_count = 0;
        header.ilk_count = 0;
        header.ich_count = 0;
        header.ich_mapping_count = 0;
        header.vocabulary_count = 0;
        header.ilk_alias_count = 0;
        header.updated_at = now_epoch_ms();
        header.heartbeat = header.updated_at;
        seqlock_end_write(&header.seq);
        Ok(())
    }

    pub fn write_snapshot_entries(
        &mut self,
        tenants_src: &[TenantEntry],
        ilks_src: &[IlkEntry],
        ichs_src: &[IchEntry],
        aliases_src: &[IlkAliasEntry],
        vocabulary_src: &[VocabularyEntry],
    ) -> Result<(), ShmError> {
        let max_tenants = self.layout.limits.max_tenants as usize;
        let max_ilks = self.layout.limits.max_ilks as usize;
        let max_ichs = self.layout.limits.max_ichs() as usize;
        let max_ilk_aliases = self.layout.limits.max_ilk_aliases as usize;
        let max_vocabulary = self.layout.limits.max_vocabulary as usize;

        if tenants_src.len() > max_tenants {
            return Err(ShmError::ValueTooLong {
                len: tenants_src.len(),
                max: max_tenants,
            });
        }
        if ilks_src.len() > max_ilks {
            return Err(ShmError::ValueTooLong {
                len: ilks_src.len(),
                max: max_ilks,
            });
        }
        if ichs_src.len() > max_ichs {
            return Err(ShmError::ValueTooLong {
                len: ichs_src.len(),
                max: max_ichs,
            });
        }
        if aliases_src.len() > max_ilk_aliases {
            return Err(ShmError::ValueTooLong {
                len: aliases_src.len(),
                max: max_ilk_aliases,
            });
        }
        if vocabulary_src.len() > max_vocabulary {
            return Err(ShmError::ValueTooLong {
                len: vocabulary_src.len(),
                max: max_vocabulary,
            });
        }

        let (header, tenants, ilks, ichs, mappings, aliases, vocabulary) =
            identity_header_and_entries_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;

        seqlock_begin_write(&header.seq);
        for entry in tenants.iter_mut() {
            *entry = empty_tenant_entry();
        }
        for entry in ilks.iter_mut() {
            *entry = empty_ilk_entry();
        }
        for entry in ichs.iter_mut() {
            *entry = empty_ich_entry();
        }
        for entry in mappings.iter_mut() {
            *entry = empty_ich_mapping_entry();
        }
        for entry in aliases.iter_mut() {
            *entry = empty_ilk_alias_entry();
        }
        for entry in vocabulary.iter_mut() {
            *entry = empty_vocabulary_entry();
        }

        for (idx, entry) in tenants_src.iter().enumerate() {
            tenants[idx] = *entry;
        }
        for (idx, entry) in ilks_src.iter().enumerate() {
            ilks[idx] = *entry;
        }
        for (idx, entry) in ichs_src.iter().enumerate() {
            ichs[idx] = *entry;
        }
        for (idx, entry) in aliases_src.iter().enumerate() {
            aliases[idx] = *entry;
        }
        for (idx, entry) in vocabulary_src.iter().enumerate() {
            vocabulary[idx] = *entry;
        }

        header.tenant_count = tenants_src.len() as u32;
        header.ilk_count = ilks_src.len() as u32;
        header.ich_count = ichs_src.len() as u32;
        header.ich_mapping_count = 0;
        header.ilk_alias_count = aliases_src.len() as u32;
        header.vocabulary_count = vocabulary_src.len() as u32;
        header.updated_at = now_epoch_ms();
        header.heartbeat = header.updated_at;
        seqlock_end_write(&header.seq);
        Ok(())
    }

    pub fn upsert_ich_mapping(
        &mut self,
        channel_type: &str,
        address: &str,
        ich_id: [u8; 16],
        ilk_id: [u8; 16],
    ) -> Result<(), ShmError> {
        if channel_type.len() > ICH_CHANNEL_TYPE_MAX_LEN {
            return Err(ShmError::ValueTooLong {
                len: channel_type.len(),
                max: ICH_CHANNEL_TYPE_MAX_LEN,
            });
        }
        if address.len() > ICH_ADDRESS_MAX_LEN {
            return Err(ShmError::ValueTooLong {
                len: address.len(),
                max: ICH_ADDRESS_MAX_LEN,
            });
        }
        let (header, _tenants, _ilks, _ichs, mappings, _aliases, _vocabulary) =
            identity_header_and_entries_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;
        let hash = compute_ich_hash(channel_type, address);
        seqlock_begin_write(&header.seq);
        let result =
            upsert_ich_mapping_entry(mappings, hash, channel_type, address, ich_id, ilk_id);
        if let Ok(inserted) = result {
            if inserted {
                header.ich_mapping_count = header.ich_mapping_count.saturating_add(1);
            }
            header.updated_at = now_epoch_ms();
            header.heartbeat = header.updated_at;
        }
        seqlock_end_write(&header.seq);
        result.map(|_| ())
    }

    pub fn remove_ich_mapping(
        &mut self,
        channel_type: &str,
        address: &str,
    ) -> Result<bool, ShmError> {
        let (header, _tenants, _ilks, _ichs, mappings, _aliases, _vocabulary) =
            identity_header_and_entries_mut(&mut self.mmap, &self.layout)
                .ok_or(ShmError::InvalidHeader)?;
        let hash = compute_ich_hash(channel_type, address);
        seqlock_begin_write(&header.seq);
        let removed = remove_ich_mapping_entry(mappings, hash, channel_type, address);
        if removed {
            header.ich_mapping_count = header.ich_mapping_count.saturating_sub(1);
            header.updated_at = now_epoch_ms();
            header.heartbeat = header.updated_at;
        }
        seqlock_end_write(&header.seq);
        Ok(removed)
    }

    pub fn resolve_ich_mapping(
        &self,
        channel_type: &str,
        address: &str,
    ) -> Option<([u8; 16], [u8; 16])> {
        let header = self.header_ref()?;
        resolve_ich_mapping_from_region(
            header,
            self.mmap.as_ref(),
            &self.layout,
            channel_type,
            address,
        )
    }

    fn header_ref(&self) -> Option<&IdentityHeader> {
        header_ref::<IdentityHeader>(self.mmap.as_ref(), self.layout.header_offset)
    }

    fn header_mut(&mut self) -> Option<&mut IdentityHeader> {
        header_mut::<IdentityHeader>(&mut self.mmap, self.layout.header_offset)
    }
}

impl IdentityRegionReader {
    pub fn open_read_only(name: &str, limits: IdentityRegionLimits) -> Result<Self, ShmError> {
        validate_name(name)?;
        let layout = layout_identity(limits);
        let mmap = open_read_only_region(name, layout.total_len)?;
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    /// Opens the identity region by discovering limits from the SHM header.
    /// Useful for IO readers that should not need static compile-time limits.
    pub fn open_read_only_auto(name: &str) -> Result<Self, ShmError> {
        validate_name(name)?;
        let cstr = CString::new(name).map_err(|_| ShmError::NameTooLong)?;
        let fd = shm_open(
            cstr.as_c_str(),
            OFlag::O_RDONLY,
            nix::sys::stat::Mode::empty(),
        )?;
        let stat = fstat(fd.as_raw_fd())?;
        if stat.st_size <= 0 {
            return Err(ShmError::InvalidHeader);
        }
        let mmap = unsafe {
            MmapOptions::new()
                .len(stat.st_size as usize)
                .map(fd.as_raw_fd())?
        };
        if !is_region_valid(mmap.as_ref()) {
            return Err(ShmError::InvalidHeader);
        }
        let header =
            header_ref::<IdentityHeader>(mmap.as_ref(), 0).ok_or(ShmError::InvalidHeader)?;
        if header.version != IDENTITY_VERSION {
            return Err(ShmError::InvalidHeader);
        }
        let limits = IdentityRegionLimits {
            max_ilks: header.max_ilks,
            max_tenants: header.max_tenants,
            max_vocabulary: header.max_vocabulary,
            max_ilk_aliases: header.max_ilk_aliases,
        };
        let layout = layout_identity(limits);
        if layout.total_len > stat.st_size as usize {
            return Err(ShmError::InvalidHeader);
        }
        Ok(Self {
            name: name.to_string(),
            mmap,
            layout,
        })
    }

    pub fn read_snapshot(&self) -> Option<IdentitySnapshot> {
        let header = self.header_ref()?;
        read_identity_snapshot(header, self.mmap.as_ref(), &self.layout)
    }

    pub fn resolve_ich_mapping(
        &self,
        channel_type: &str,
        address: &str,
    ) -> Option<([u8; 16], [u8; 16])> {
        let header = self.header_ref()?;
        resolve_ich_mapping_from_region(
            header,
            self.mmap.as_ref(),
            &self.layout,
            channel_type,
            address,
        )
    }

    fn header_ref(&self) -> Option<&IdentityHeader> {
        header_ref::<IdentityHeader>(self.mmap.as_ref(), self.layout.header_offset)
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

pub fn compute_ich_hash(channel_type: &str, address: &str) -> u64 {
    // FNV-1a 64-bit, deterministic across processes/hives.
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;

    let mut hash = FNV_OFFSET_BASIS;
    for b in channel_type.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    // Delimiter to avoid ambiguous concatenations.
    hash ^= 0xff;
    hash = hash.wrapping_mul(FNV_PRIME);
    for b in address.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
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

fn layout_identity(limits: IdentityRegionLimits) -> IdentityRegionLayout {
    let max_tenants = limits.max_tenants as usize;
    let max_ilks = limits.max_ilks as usize;
    let max_ichs = limits.max_ichs() as usize;
    let max_ich_mappings = limits.max_ich_mappings() as usize;
    let max_ilk_aliases = limits.max_ilk_aliases as usize;
    let max_vocabulary = limits.max_vocabulary as usize;

    let header_size = size_of::<IdentityHeader>();
    let tenant_size = size_of::<TenantEntry>() * max_tenants;
    let ilk_size = size_of::<IlkEntry>() * max_ilks;
    let ich_size = size_of::<IchEntry>() * max_ichs;
    let mapping_size = size_of::<IchMappingEntry>() * max_ich_mappings;
    let alias_size = size_of::<IlkAliasEntry>() * max_ilk_aliases;
    let vocabulary_size = size_of::<VocabularyEntry>() * max_vocabulary;

    let header_offset = 0;
    let tenant_offset = align_up(header_offset + header_size, REGION_ALIGNMENT);
    let ilk_offset = align_up(tenant_offset + tenant_size, REGION_ALIGNMENT);
    let ich_offset = align_up(ilk_offset + ilk_size, REGION_ALIGNMENT);
    let ich_mapping_offset = align_up(ich_offset + ich_size, REGION_ALIGNMENT);
    let ilk_alias_offset = align_up(ich_mapping_offset + mapping_size, REGION_ALIGNMENT);
    let vocabulary_offset = align_up(ilk_alias_offset + alias_size, REGION_ALIGNMENT);
    let variable_offset = align_up(vocabulary_offset + vocabulary_size, REGION_ALIGNMENT);
    let total_len = variable_offset;

    IdentityRegionLayout {
        header_offset,
        tenant_offset,
        ilk_offset,
        ich_offset,
        ich_mapping_offset,
        ilk_alias_offset,
        vocabulary_offset,
        variable_offset,
        total_len,
        limits,
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
        IDENTITY_MAGIC => header_ref::<IdentityHeader>(mmap, 0)
            .is_some_and(|header| header.version == IDENTITY_VERSION),
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
        header.local_hive_id_len = copy_bytes_with_len(&mut header.local_hive_id, hive_id) as u16;
        header.created_at = now_epoch_ms();
        header.updated_at = header.created_at;
    }
}

fn initialize_identity_header(
    mmap: &mut MmapMut,
    owner_uuid: Uuid,
    hive_id: &str,
    is_primary: bool,
    limits: IdentityRegionLimits,
) {
    if let Some(header) = header_mut::<IdentityHeader>(mmap, 0) {
        header.magic = IDENTITY_MAGIC;
        header.version = IDENTITY_VERSION;
        header.seq = AtomicU64::new(0);

        header.tenant_count = 0;
        header.ilk_count = 0;
        header.ich_count = 0;
        header.ich_mapping_count = 0;
        header.vocabulary_count = 0;
        header.ilk_alias_count = 0;

        header.max_ilks = limits.max_ilks;
        header.max_tenants = limits.max_tenants;
        header.max_ichs = limits.max_ichs();
        header.max_ich_mappings = limits.max_ich_mappings();
        header.max_vocabulary = limits.max_vocabulary;
        header.max_ilk_aliases = limits.max_ilk_aliases;

        header.updated_at = now_epoch_ms();
        header.heartbeat = header.updated_at;

        header.owner_uuid = *owner_uuid.as_bytes();
        header.owner_pid = std::process::id();
        header.is_primary = if is_primary { 1 } else { 0 };
        header.hive_id_len = copy_bytes_with_len(&mut header.hive_id, hive_id) as u16;
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

        let hives =
            read_slice::<RemoteHiveEntry>(mmap, layout.hive_offset, MAX_REMOTE_HIVES as usize)?;
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

fn read_identity_snapshot(
    header: &IdentityHeader,
    mmap: &[u8],
    layout: &IdentityRegionLayout,
) -> Option<IdentitySnapshot> {
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

        let tenant_count = header.tenant_count as usize;
        let ilk_count = header.ilk_count as usize;
        let ich_count = header.ich_count as usize;
        let ich_mapping_count = header.ich_mapping_count as usize;
        let vocabulary_count = header.vocabulary_count as usize;
        let ilk_alias_count = header.ilk_alias_count as usize;

        let max_tenants = layout.limits.max_tenants as usize;
        let max_ilks = layout.limits.max_ilks as usize;
        let max_ichs = layout.limits.max_ichs() as usize;
        let max_ich_mappings = layout.limits.max_ich_mappings() as usize;
        let max_vocabulary = layout.limits.max_vocabulary as usize;
        let max_ilk_aliases = layout.limits.max_ilk_aliases as usize;

        if tenant_count > max_tenants
            || ilk_count > max_ilks
            || ich_count > max_ichs
            || ich_mapping_count > max_ich_mappings
            || vocabulary_count > max_vocabulary
            || ilk_alias_count > max_ilk_aliases
        {
            return None;
        }

        let tenants = read_slice::<TenantEntry>(mmap, layout.tenant_offset, max_tenants)?;
        let ilks = read_slice::<IlkEntry>(mmap, layout.ilk_offset, max_ilks)?;
        let ichs = read_slice::<IchEntry>(mmap, layout.ich_offset, max_ichs)?;
        let ich_mappings =
            read_slice::<IchMappingEntry>(mmap, layout.ich_mapping_offset, max_ich_mappings)?;
        let ilk_aliases =
            read_slice::<IlkAliasEntry>(mmap, layout.ilk_alias_offset, max_ilk_aliases)?;
        let vocabulary =
            read_slice::<VocabularyEntry>(mmap, layout.vocabulary_offset, max_vocabulary)?;

        let mut tenant_snapshot = Vec::with_capacity(tenant_count);
        for entry in tenants.iter().take(tenant_count) {
            tenant_snapshot.push(*entry);
        }
        let mut ilk_snapshot = Vec::with_capacity(ilk_count);
        for entry in ilks.iter().take(ilk_count) {
            ilk_snapshot.push(*entry);
        }
        let mut ich_snapshot = Vec::with_capacity(ich_count);
        for entry in ichs.iter().take(ich_count) {
            ich_snapshot.push(*entry);
        }
        let mut mapping_snapshot = Vec::with_capacity(ich_mapping_count);
        for entry in ich_mappings.iter().take(ich_mapping_count) {
            mapping_snapshot.push(*entry);
        }
        let mut alias_snapshot = Vec::with_capacity(ilk_alias_count);
        for entry in ilk_aliases.iter().take(ilk_alias_count) {
            alias_snapshot.push(*entry);
        }
        let mut vocabulary_snapshot = Vec::with_capacity(vocabulary_count);
        for entry in vocabulary.iter().take(vocabulary_count) {
            vocabulary_snapshot.push(*entry);
        }

        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return Some(IdentitySnapshot {
                header: IdentityHeaderSnapshot {
                    tenant_count: header.tenant_count,
                    ilk_count: header.ilk_count,
                    ich_count: header.ich_count,
                    ich_mapping_count: header.ich_mapping_count,
                    vocabulary_count: header.vocabulary_count,
                    ilk_alias_count: header.ilk_alias_count,
                    max_ilks: header.max_ilks,
                    max_tenants: header.max_tenants,
                    max_ichs: header.max_ichs,
                    max_ich_mappings: header.max_ich_mappings,
                    max_vocabulary: header.max_vocabulary,
                    max_ilk_aliases: header.max_ilk_aliases,
                    heartbeat: header.heartbeat,
                    updated_at: header.updated_at,
                },
                tenants: tenant_snapshot,
                ilks: ilk_snapshot,
                ichs: ich_snapshot,
                ich_mappings: mapping_snapshot,
                ilk_aliases: alias_snapshot,
                vocabulary: vocabulary_snapshot,
            });
        }
    }
}

fn resolve_ich_mapping_from_region(
    header: &IdentityHeader,
    mmap: &[u8],
    layout: &IdentityRegionLayout,
    channel_type: &str,
    address: &str,
) -> Option<([u8; 16], [u8; 16])> {
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
        let mappings = read_slice::<IchMappingEntry>(
            mmap,
            layout.ich_mapping_offset,
            layout.limits.max_ich_mappings() as usize,
        )?;
        let resolved = resolve_ich_mapping_from_entries(mappings, channel_type, address);
        atomic::fence(Ordering::Acquire);
        let s2 = header.seq.load(Ordering::Acquire);
        if s1 == s2 {
            return resolved;
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

#[allow(clippy::type_complexity)]
fn identity_header_and_entries_mut<'a>(
    mmap: &'a mut MmapMut,
    layout: &IdentityRegionLayout,
) -> Option<(
    &'a mut IdentityHeader,
    &'a mut [TenantEntry],
    &'a mut [IlkEntry],
    &'a mut [IchEntry],
    &'a mut [IchMappingEntry],
    &'a mut [IlkAliasEntry],
    &'a mut [VocabularyEntry],
)> {
    let header_offset = layout.header_offset;
    let tenants_offset = layout.tenant_offset;
    let ilks_offset = layout.ilk_offset;
    let ichs_offset = layout.ich_offset;
    let mappings_offset = layout.ich_mapping_offset;
    let aliases_offset = layout.ilk_alias_offset;
    let vocabulary_offset = layout.vocabulary_offset;

    let max_tenants = layout.limits.max_tenants as usize;
    let max_ilks = layout.limits.max_ilks as usize;
    let max_ichs = layout.limits.max_ichs() as usize;
    let max_ich_mappings = layout.limits.max_ich_mappings() as usize;
    let max_ilk_aliases = layout.limits.max_ilk_aliases as usize;
    let max_vocabulary = layout.limits.max_vocabulary as usize;

    let len = mmap.len();
    let base = mmap.as_mut_ptr();
    if header_offset + size_of::<IdentityHeader>() > len {
        return None;
    }
    if vocabulary_offset + size_of::<VocabularyEntry>() * max_vocabulary > len {
        return None;
    }
    unsafe {
        let header_ptr = base.add(header_offset) as *mut IdentityHeader;
        let tenants_ptr = base.add(tenants_offset) as *mut TenantEntry;
        let ilks_ptr = base.add(ilks_offset) as *mut IlkEntry;
        let ichs_ptr = base.add(ichs_offset) as *mut IchEntry;
        let mappings_ptr = base.add(mappings_offset) as *mut IchMappingEntry;
        let aliases_ptr = base.add(aliases_offset) as *mut IlkAliasEntry;
        let vocabulary_ptr = base.add(vocabulary_offset) as *mut VocabularyEntry;
        Some((
            &mut *header_ptr,
            std::slice::from_raw_parts_mut(tenants_ptr, max_tenants),
            std::slice::from_raw_parts_mut(ilks_ptr, max_ilks),
            std::slice::from_raw_parts_mut(ichs_ptr, max_ichs),
            std::slice::from_raw_parts_mut(mappings_ptr, max_ich_mappings),
            std::slice::from_raw_parts_mut(aliases_ptr, max_ilk_aliases),
            std::slice::from_raw_parts_mut(vocabulary_ptr, max_vocabulary),
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

fn upsert_ich_mapping_entry(
    mappings: &mut [IchMappingEntry],
    hash: u64,
    channel_type: &str,
    address: &str,
    ich_id: [u8; 16],
    ilk_id: [u8; 16],
) -> Result<bool, ShmError> {
    let table_len = mappings.len();
    if table_len == 0 {
        return Err(ShmError::SlotFull);
    }
    let start = (hash as usize) % table_len;
    let mut first_tombstone: Option<usize> = None;

    for probe in 0..table_len {
        let idx = (start + probe) % table_len;
        let flags = mappings[idx].flags;

        if flags == 0 {
            let target = first_tombstone.unwrap_or(idx);
            write_ich_mapping_entry(
                &mut mappings[target],
                hash,
                channel_type,
                address,
                ich_id,
                ilk_id,
            );
            return Ok(true);
        }
        if flags & ICH_MAP_FLAG_TOMBSTONE != 0 {
            if first_tombstone.is_none() {
                first_tombstone = Some(idx);
            }
            continue;
        }
        if flags & ICH_MAP_FLAG_OCCUPIED != 0
            && mappings[idx].hash == hash
            && fixed_str_matches(&mappings[idx].channel_type, channel_type)
            && fixed_str_matches(&mappings[idx].address, address)
        {
            write_ich_mapping_entry(
                &mut mappings[idx],
                hash,
                channel_type,
                address,
                ich_id,
                ilk_id,
            );
            return Ok(false);
        }
    }

    if let Some(target) = first_tombstone {
        write_ich_mapping_entry(
            &mut mappings[target],
            hash,
            channel_type,
            address,
            ich_id,
            ilk_id,
        );
        return Ok(true);
    }

    Err(ShmError::SlotFull)
}

fn remove_ich_mapping_entry(
    mappings: &mut [IchMappingEntry],
    hash: u64,
    channel_type: &str,
    address: &str,
) -> bool {
    let table_len = mappings.len();
    if table_len == 0 {
        return false;
    }
    let start = (hash as usize) % table_len;

    for probe in 0..table_len {
        let idx = (start + probe) % table_len;
        let flags = mappings[idx].flags;
        if flags == 0 {
            return false;
        }
        if flags & ICH_MAP_FLAG_TOMBSTONE != 0 {
            continue;
        }
        if flags & ICH_MAP_FLAG_OCCUPIED != 0
            && mappings[idx].hash == hash
            && fixed_str_matches(&mappings[idx].channel_type, channel_type)
            && fixed_str_matches(&mappings[idx].address, address)
        {
            mappings[idx] = empty_ich_mapping_entry();
            mappings[idx].hash = hash;
            mappings[idx].flags = ICH_MAP_FLAG_TOMBSTONE;
            return true;
        }
    }

    false
}

fn resolve_ich_mapping_from_entries(
    mappings: &[IchMappingEntry],
    channel_type: &str,
    address: &str,
) -> Option<([u8; 16], [u8; 16])> {
    let table_len = mappings.len();
    if table_len == 0 {
        return None;
    }
    let hash = compute_ich_hash(channel_type, address);
    let start = (hash as usize) % table_len;

    for probe in 0..table_len {
        let idx = (start + probe) % table_len;
        let entry = &mappings[idx];
        let flags = entry.flags;
        if flags == 0 {
            return None;
        }
        if flags & ICH_MAP_FLAG_TOMBSTONE != 0 {
            continue;
        }
        if flags & ICH_MAP_FLAG_OCCUPIED != 0
            && entry.hash == hash
            && fixed_str_matches(&entry.channel_type, channel_type)
            && fixed_str_matches(&entry.address, address)
        {
            return Some((entry.ich_id, entry.ilk_id));
        }
    }

    None
}

fn write_ich_mapping_entry(
    entry: &mut IchMappingEntry,
    hash: u64,
    channel_type: &str,
    address: &str,
    ich_id: [u8; 16],
    ilk_id: [u8; 16],
) {
    *entry = empty_ich_mapping_entry();
    entry.hash = hash;
    copy_bytes_with_len(&mut entry.channel_type, channel_type);
    copy_bytes_with_len(&mut entry.address, address);
    entry.ich_id = ich_id;
    entry.ilk_id = ilk_id;
    entry.flags = ICH_MAP_FLAG_OCCUPIED;
}

fn fixed_str_matches(buf: &[u8], value: &str) -> bool {
    let used = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    &buf[..used] == value.as_bytes()
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
        router_uuid: [0u8; 16],
        router_name: [0u8; 64],
        router_name_len: 0,
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

fn empty_tenant_entry() -> TenantEntry {
    TenantEntry {
        tenant_id: [0u8; 16],
        name: [0u8; 128],
        domain: [0u8; 128],
        status: 0,
        flags: 0,
        _pad0: [0u8; 5],
        max_ilks: 0,
        created_at: 0,
        updated_at: 0,
        _reserved: [0u8; 8],
    }
}

fn empty_ilk_entry() -> IlkEntry {
    IlkEntry {
        ilk_id: [0u8; 16],
        ilk_type: 0,
        registration_status: 0,
        flags: 0,
        tenant_id: [0u8; 16],
        display_name: [0u8; 128],
        handler_node: [0u8; 128],
        ich_offset: 0,
        ich_count: 0,
        _pad0: [0u8; 2],
        roles_offset: 0,
        roles_len: 0,
        _pad1: [0u8; 2],
        capabilities_offset: 0,
        capabilities_len: 0,
        _pad2: [0u8; 2],
        created_at: 0,
        updated_at: 0,
        _reserved: [0u8; 8],
    }
}

fn empty_ich_entry() -> IchEntry {
    IchEntry {
        ich_id: [0u8; 16],
        ilk_id: [0u8; 16],
        channel_type: [0u8; ICH_CHANNEL_TYPE_MAX_LEN],
        address: [0u8; ICH_ADDRESS_MAX_LEN],
        flags: 0,
        is_primary: 0,
        _pad0: [0u8; 5],
        added_at: 0,
        _reserved: [0u8; 16],
    }
}

fn empty_ich_mapping_entry() -> IchMappingEntry {
    IchMappingEntry {
        hash: 0,
        channel_type: [0u8; ICH_CHANNEL_TYPE_MAX_LEN],
        address: [0u8; ICH_ADDRESS_MAX_LEN],
        ich_id: [0u8; 16],
        ilk_id: [0u8; 16],
        flags: 0,
        _reserved: [0u8; 54],
    }
}

fn empty_ilk_alias_entry() -> IlkAliasEntry {
    IlkAliasEntry {
        old_ilk_id: [0u8; 16],
        canonical_ilk_id: [0u8; 16],
        expires_at: 0,
        flags: 0,
        _reserved: [0u8; 22],
    }
}

fn empty_vocabulary_entry() -> VocabularyEntry {
    VocabularyEntry {
        tag: [0u8; 64],
        tag_len: 0,
        category: 0,
        flags: 0,
        description: [0u8; 128],
        description_len: 0,
        _pad0: [0u8; 6],
        created_at: 0,
        deprecated_at: 0,
        _reserved: [0u8; 8],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::mman::shm_unlink;
    use std::ffi::CString;

    fn cleanup_shm(name: &str) {
        if let Ok(cstr) = CString::new(name) {
            let _ = shm_unlink(cstr.as_c_str());
        }
    }

    #[test]
    fn identity_ich_mapping_upsert_update_remove() {
        let id = Uuid::new_v4().simple().to_string();
        let name = format!("/jsid-u-{}", &id[..8]);
        cleanup_shm(&name);
        let limits = IdentityRegionLimits {
            max_ilks: 1,
            max_tenants: 1,
            max_vocabulary: 1,
            max_ilk_aliases: 1,
        };
        let mut writer =
            IdentityRegionWriter::open_or_create(&name, Uuid::new_v4(), "sandbox", true, limits)
                .expect("open identity region");

        let channel = "whatsapp";
        let address = "+5491111111111";
        let ich_a = [1u8; 16];
        let ilk_a = [2u8; 16];
        let ich_b = [3u8; 16];
        let ilk_b = [4u8; 16];

        writer
            .upsert_ich_mapping(channel, address, ich_a, ilk_a)
            .expect("insert mapping");
        let snap = writer.read_snapshot().expect("snapshot");
        assert_eq!(snap.header.ich_mapping_count, 1);
        assert_eq!(
            writer.resolve_ich_mapping(channel, address),
            Some((ich_a, ilk_a))
        );

        writer
            .upsert_ich_mapping(channel, address, ich_b, ilk_b)
            .expect("update mapping");
        let snap = writer.read_snapshot().expect("snapshot");
        assert_eq!(snap.header.ich_mapping_count, 1);
        assert_eq!(
            writer.resolve_ich_mapping(channel, address),
            Some((ich_b, ilk_b))
        );

        assert!(writer
            .remove_ich_mapping(channel, address)
            .expect("remove mapping"));
        let snap = writer.read_snapshot().expect("snapshot");
        assert_eq!(snap.header.ich_mapping_count, 0);
        assert_eq!(writer.resolve_ich_mapping(channel, address), None);
        assert!(!writer
            .remove_ich_mapping(channel, address)
            .expect("remove missing mapping"));

        cleanup_shm(&name);
    }

    #[test]
    fn identity_ich_mapping_reuses_tombstone_slot() {
        let id = Uuid::new_v4().simple().to_string();
        let name = format!("/jsid-t-{}", &id[..8]);
        cleanup_shm(&name);
        let limits = IdentityRegionLimits {
            max_ilks: 1,
            max_tenants: 1,
            max_vocabulary: 1,
            max_ilk_aliases: 1,
        };
        let table_len = limits.max_ich_mappings() as usize;
        let channel = "whatsapp";
        let first = "slot-a";
        let bucket = (compute_ich_hash(channel, first) as usize) % table_len;

        let mut colliders = vec![first.to_string()];
        for i in 0..100_000u32 {
            let candidate = format!("slot-{i}");
            if (compute_ich_hash(channel, &candidate) as usize) % table_len == bucket {
                colliders.push(candidate);
                if colliders.len() > table_len {
                    break;
                }
            }
        }
        assert!(
            colliders.len() > table_len,
            "need enough colliders for test"
        );

        let mut writer =
            IdentityRegionWriter::open_or_create(&name, Uuid::new_v4(), "sandbox", true, limits)
                .expect("open identity region");

        for (idx, address) in colliders.iter().take(table_len).enumerate() {
            writer
                .upsert_ich_mapping(channel, address, [idx as u8; 16], [200u8 + idx as u8; 16])
                .expect("fill table");
        }
        assert_eq!(
            writer
                .read_snapshot()
                .expect("snapshot")
                .header
                .ich_mapping_count as usize,
            table_len
        );

        let overflow =
            writer.upsert_ich_mapping(channel, &colliders[table_len], [250u8; 16], [251u8; 16]);
        assert!(matches!(overflow, Err(ShmError::SlotFull)));

        assert!(writer.remove_ich_mapping(channel, first).expect("remove A"));
        writer
            .upsert_ich_mapping(channel, &colliders[table_len], [12u8; 16], [13u8; 16])
            .expect("insert B");
        assert_eq!(
            writer
                .read_snapshot()
                .expect("snapshot")
                .header
                .ich_mapping_count as usize,
            table_len
        );
        assert_eq!(
            writer.resolve_ich_mapping(channel, &colliders[table_len]),
            Some(([12u8; 16], [13u8; 16]))
        );

        cleanup_shm(&name);
    }

    #[test]
    fn identity_reader_auto_discovers_limits_from_header() {
        let name = format!("/jat{}", &Uuid::new_v4().simple().to_string()[..8]);
        let limits = IdentityRegionLimits {
            max_ilks: 2,
            max_tenants: 2,
            max_vocabulary: 8,
            max_ilk_aliases: 2,
        };
        let mut writer =
            IdentityRegionWriter::open_or_create(&name, Uuid::new_v4(), "sandbox", true, limits)
                .expect("open identity region");
        writer
            .upsert_ich_mapping("whatsapp", "+549111111", [1u8; 16], [2u8; 16])
            .expect("upsert mapping");

        let reader = IdentityRegionReader::open_read_only_auto(&name).expect("open auto reader");
        assert_eq!(
            reader.resolve_ich_mapping("whatsapp", "+549111111"),
            Some(([1u8; 16], [2u8; 16]))
        );

        cleanup_shm(&name);
    }

    #[test]
    fn identity_write_snapshot_entries_sets_counts_and_resets_mappings() {
        let name = format!("/jsid-s-{}", &Uuid::new_v4().simple().to_string()[..8]);
        cleanup_shm(&name);
        let limits = IdentityRegionLimits {
            max_ilks: 4,
            max_tenants: 4,
            max_vocabulary: 4,
            max_ilk_aliases: 4,
        };
        let mut writer =
            IdentityRegionWriter::open_or_create(&name, Uuid::new_v4(), "sandbox", true, limits)
                .expect("open identity region");
        writer
            .upsert_ich_mapping("whatsapp", "+549111111", [1u8; 16], [2u8; 16])
            .expect("seed mapping");

        let tenant = TenantEntry {
            tenant_id: [3u8; 16],
            name: [0u8; 128],
            domain: [0u8; 128],
            status: 1,
            flags: FLAG_ACTIVE,
            _pad0: [0u8; 5],
            max_ilks: 100,
            created_at: 1,
            updated_at: 1,
            _reserved: [0u8; 8],
        };
        let ilk = IlkEntry {
            ilk_id: [4u8; 16],
            ilk_type: 1,
            registration_status: 2,
            flags: FLAG_ACTIVE,
            tenant_id: [3u8; 16],
            display_name: [0u8; 128],
            handler_node: [0u8; 128],
            ich_offset: 0,
            ich_count: 1,
            _pad0: [0u8; 2],
            roles_offset: 0,
            roles_len: 0,
            _pad1: [0u8; 2],
            capabilities_offset: 0,
            capabilities_len: 0,
            _pad2: [0u8; 2],
            created_at: 1,
            updated_at: 1,
            _reserved: [0u8; 8],
        };
        let ich = IchEntry {
            ich_id: [5u8; 16],
            ilk_id: [4u8; 16],
            channel_type: [0u8; ICH_CHANNEL_TYPE_MAX_LEN],
            address: [0u8; ICH_ADDRESS_MAX_LEN],
            flags: FLAG_ACTIVE,
            is_primary: 1,
            _pad0: [0u8; 5],
            added_at: 1,
            _reserved: [0u8; 16],
        };
        let alias = IlkAliasEntry {
            old_ilk_id: [6u8; 16],
            canonical_ilk_id: [4u8; 16],
            expires_at: 2,
            flags: FLAG_ACTIVE,
            _reserved: [0u8; 22],
        };

        writer
            .write_snapshot_entries(&[tenant], &[ilk], &[ich], &[alias], &[])
            .expect("write snapshot entries");
        let snap = writer.read_snapshot().expect("snapshot");
        assert_eq!(snap.header.tenant_count, 1);
        assert_eq!(snap.header.ilk_count, 1);
        assert_eq!(snap.header.ich_count, 1);
        assert_eq!(snap.header.ilk_alias_count, 1);
        assert_eq!(snap.header.ich_mapping_count, 0);
        assert_eq!(writer.resolve_ich_mapping("whatsapp", "+549111111"), None);

        cleanup_shm(&name);
    }
}
