//! How many connections, or requests, one peer address may hold at once: the network doors' share
//! per client (sy-edge's door and `/public/`, FINDINGS A-86; the proxy to io.web, A-90; io.web's
//! handshakes, A-89).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex, PoisonError};

/// How many connections, or downloads, one peer address may hold at once (A-86). An IPv6 peer
/// counts by its /64, the block one subscriber gets; an IPv4-mapped address as its IPv4.
pub struct PeerSlots {
    /// 0: no cap.
    max: usize,
    held: Mutex<HashMap<IpAddr, usize>>,
}

/// One slot of a `PeerSlots`, given back when dropped.
pub struct PeerSlot {
    slots: Arc<PeerSlots>,
    /// `None` when there is no cap to count against.
    key: Option<IpAddr>,
}

impl PeerSlots {
    pub fn new(max: usize) -> Arc<Self> {
        Arc::new(Self {
            max,
            held: Mutex::new(HashMap::new()),
        })
    }

    /// A slot for `peer`, or `None` when it holds `max` already.
    pub fn try_take(self: &Arc<Self>, peer: IpAddr) -> Option<PeerSlot> {
        if self.max == 0 {
            return Some(PeerSlot {
                slots: Arc::clone(self),
                key: None,
            });
        }
        let key = match peer.to_canonical() {
            IpAddr::V6(v6) => {
                let [a, b, c, d, ..] = v6.segments();
                IpAddr::V6(Ipv6Addr::new(a, b, c, d, 0, 0, 0, 0))
            }
            v4 => v4,
        };
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let count = held.entry(key).or_insert(0);
        if *count >= self.max {
            return None;
        }
        *count += 1;
        Some(PeerSlot {
            slots: Arc::clone(self),
            key: Some(key),
        })
    }

    /// The cap: the slots one address may hold (0: none).
    pub fn max(&self) -> usize {
        self.max
    }

    /// The slots `peer`'s address (its /64, for IPv6) holds now.
    pub fn held_by(&self, peer: IpAddr) -> usize {
        let held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        held.get(&peer).copied().unwrap_or(0)
    }
}

impl Drop for PeerSlot {
    fn drop(&mut self) {
        let Some(key) = self.key else {
            return;
        };
        let mut held = self
            .slots
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = held.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                held.remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::PeerSlots;
    use std::net::IpAddr;

    #[test]
    fn peer_slots_count_an_address_its_ipv6_64_and_give_slots_back() {
        let ip = |text: &str| text.parse::<IpAddr>().expect("ip");
        let slots = PeerSlots::new(2);
        let first = slots.try_take(ip("198.51.100.4")).expect("first");
        let _second = slots
            .try_take(ip("::ffff:198.51.100.4"))
            .expect("the same IPv4");
        assert!(slots.try_take(ip("198.51.100.4")).is_none(), "at the cap");
        assert!(
            slots.try_take(ip("198.51.100.5")).is_some(),
            "another address"
        );
        drop(first);
        assert_eq!(slots.held_by(ip("198.51.100.4")), 1);
        assert!(
            slots.try_take(ip("198.51.100.4")).is_some(),
            "the slot came back"
        );

        // One IPv6 subscriber holds a /64: its addresses count together.
        let _a = slots.try_take(ip("2001:db8:1:2::1")).expect("a");
        let _b = slots.try_take(ip("2001:db8:1:2:ffff::9")).expect("b");
        assert!(slots.try_take(ip("2001:db8:1:2::77")).is_none());
        assert!(
            slots.try_take(ip("2001:db8:1:3::1")).is_some(),
            "another /64"
        );
        assert_eq!(slots.held_by(ip("2001:db8:1:2::")), 2);

        // 0: no cap, and nothing counted.
        let open = PeerSlots::new(0);
        let held: Vec<_> = (0..1000)
            .map(|_| open.try_take(ip("198.51.100.4")).expect("no cap"))
            .collect();
        assert_eq!(held.len(), 1000);
        assert_eq!(open.held_by(ip("198.51.100.4")), 0);
    }
}
