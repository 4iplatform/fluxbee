use crate::shm::{HasFlags, FLAG_ACTIVE, FLAG_DELETED};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct NodeEntry {
    pub uuid: [u8; 16],
    pub name: [u8; 256],
    pub name_len: u16,
    pub flags: u16,
    pub connected_at: u64,
    pub _reserved: [u8; 28],
}

impl Default for NodeEntry {
    fn default() -> Self {
        Self {
            uuid: [0u8; 16],
            name: [0u8; 256],
            name_len: 0,
            flags: 0,
            connected_at: 0,
            _reserved: [0u8; 28],
        }
    }
}

impl NodeEntry {
    pub fn is_active(&self) -> bool {
        (self.flags & FLAG_ACTIVE) != 0 && (self.flags & FLAG_DELETED) == 0
    }
}

impl HasFlags for NodeEntry {
    fn flags(&self) -> u16 {
        self.flags
    }
}
