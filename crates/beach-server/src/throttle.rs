//! Slows down password guessing. After a failed authentication the peer's
//! address is turned away for a while; every further failure doubles that,
//! up to [`MAX_BLOCK`]. A VNC password has eight DES-keyed characters, so
//! unthrottled guessing over the network would be feasible.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Block after the first failure.
pub const FIRST_BLOCK: Duration = Duration::from_secs(10);
/// Longest block, however many failures.
pub const MAX_BLOCK: Duration = Duration::from_secs(10 * 60);
/// A peer that stayed away this long starts afresh.
const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);
/// Addresses remembered at most; the oldest entries go first.
const MAX_ENTRIES: usize = 4096;

struct Entry {
    failures: u32,
    last_failure: Instant,
    blocked_until: Instant,
}

#[derive(Default)]
pub struct Throttle {
    peers: Mutex<HashMap<IpAddr, Entry>>,
}

impl Throttle {
    /// How much longer `peer` has to wait, if it is blocked.
    pub fn blocked(&self, peer: IpAddr) -> Option<Duration> {
        self.blocked_at(peer, Instant::now())
    }

    fn blocked_at(&self, peer: IpAddr, now: Instant) -> Option<Duration> {
        let peers = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = peers.get(&peer)?;
        (entry.blocked_until > now).then(|| entry.blocked_until - now)
    }

    /// Records a failed authentication; returns how long the peer is blocked.
    pub fn failed(&self, peer: IpAddr) -> Duration {
        self.failed_at(peer, Instant::now())
    }

    fn failed_at(&self, peer: IpAddr, now: Instant) -> Duration {
        let mut peers = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        peers.retain(|_, e| now.duration_since(e.last_failure) < FORGET_AFTER);
        if peers.len() >= MAX_ENTRIES && !peers.contains_key(&peer) {
            if let Some(oldest) = peers
                .iter()
                .min_by_key(|(_, e)| e.last_failure)
                .map(|(ip, _)| *ip)
            {
                peers.remove(&oldest);
            }
        }
        let entry = peers.entry(peer).or_insert(Entry {
            failures: 0,
            last_failure: now,
            blocked_until: now,
        });
        entry.failures = entry.failures.saturating_add(1);
        entry.last_failure = now;
        let block = FIRST_BLOCK
            .checked_mul(1u32.checked_shl(entry.failures - 1).unwrap_or(u32::MAX))
            .unwrap_or(MAX_BLOCK)
            .min(MAX_BLOCK);
        entry.blocked_until = now + block;
        block
    }

    /// A successful authentication clears the peer's record.
    pub fn succeeded(&self, peer: IpAddr) {
        let mut peers = self
            .peers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        peers.remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    #[test]
    fn blocks_grow_and_clear() {
        let t = Throttle::default();
        let now = Instant::now();
        assert_eq!(t.blocked_at(ip(1), now), None);
        assert_eq!(t.failed_at(ip(1), now), FIRST_BLOCK);
        assert_eq!(
            t.blocked_at(ip(1), now + Duration::from_secs(5)),
            Some(Duration::from_secs(5))
        );
        assert_eq!(t.blocked_at(ip(1), now + FIRST_BLOCK), None);
        assert_eq!(t.blocked_at(ip(2), now), None, "other peers aren't affected");
        assert_eq!(t.failed_at(ip(1), now + FIRST_BLOCK), FIRST_BLOCK * 2);
        assert_eq!(t.failed_at(ip(1), now + FIRST_BLOCK * 3), FIRST_BLOCK * 4);
        for _ in 0..40 {
            assert!(t.failed_at(ip(1), now + FIRST_BLOCK * 3) <= MAX_BLOCK);
        }
        assert_eq!(t.failed_at(ip(1), now + FIRST_BLOCK * 3), MAX_BLOCK);
        t.succeeded(ip(1));
        assert_eq!(t.blocked_at(ip(1), now + FIRST_BLOCK * 3), None);
        assert_eq!(t.failed_at(ip(1), now + FIRST_BLOCK * 3), FIRST_BLOCK);
    }

    #[test]
    fn old_records_are_forgotten_and_the_table_is_bounded() {
        let t = Throttle::default();
        let now = Instant::now();
        t.failed_at(ip(1), now);
        t.failed_at(ip(1), now);
        assert_eq!(
            t.failed_at(ip(1), now + FORGET_AFTER),
            FIRST_BLOCK,
            "started afresh"
        );
        for i in 0..=255u8 {
            for j in 0..=255u8 {
                t.failed_at(IpAddr::from([10, 1, i, j]), now + FORGET_AFTER);
            }
        }
        assert!(t.peers.lock().unwrap().len() <= MAX_ENTRIES);
    }
}
