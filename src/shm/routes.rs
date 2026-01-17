use crate::shm::{HasFlags, FLAG_ACTIVE, FLAG_DELETED};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct RouteEntry {
    pub prefix: [u8; 256],
    pub prefix_len: u16,
    pub match_kind: u8,
    pub route_type: u8,

    pub next_hop_router: [u8; 16],
    pub out_link: u32,
    pub metric: u32,

    pub admin_distance: u16,
    pub flags: u16,

    pub installed_at: u64,
    pub _reserved: [u8; 8],
}

impl Default for RouteEntry {
    fn default() -> Self {
        Self {
            prefix: [0u8; 256],
            prefix_len: 0,
            match_kind: 0,
            route_type: 0,
            next_hop_router: [0u8; 16],
            out_link: 0,
            metric: 0,
            admin_distance: 0,
            flags: 0,
            installed_at: 0,
            _reserved: [0u8; 8],
        }
    }
}

impl RouteEntry {
    pub fn is_active(&self) -> bool {
        (self.flags & FLAG_ACTIVE) != 0 && (self.flags & FLAG_DELETED) == 0
    }
}

impl HasFlags for RouteEntry {
    fn flags(&self) -> u16 {
        self.flags
    }
}
