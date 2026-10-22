//! Store-and-forward state: pinned keys of peers we have met, and packets held
//! in custody for peers that are currently out of range.
//!
//! Both survive restarts when the mesh has a `state_dir`, which matters for the
//! field case: a Pi in a backpack meets a phone, goes offline, and delivers the
//! phone's messages hours later.

use amara_core::constants::{MAX_STORED_MESSAGES, STORE_FORWARD_TTL_SECS};
use amara_core::peer::PeerId;
use amara_core::types::now_timestamp;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tracing::warn;

/// Most peers remembered; the least recently seen are forgotten first.
pub const MAX_KNOWN_PEERS: usize = 4096;
/// Most packets held for any single destination.
pub const MAX_STORED_PER_PEER: usize = 100;

/// Keys pinned from a verified announce or envelope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KnownPeer {
    #[serde(with = "hex_key")]
    pub ed25519: [u8; 32],
    #[serde(with = "hex_key")]
    pub x25519: [u8; 32],
    pub name: String,
    /// Unix seconds.
    pub last_seen: u32,
}

/// Peers we have verified at least once, including ones that have gone offline.
#[derive(Default)]
pub struct KnownPeers {
    peers: Mutex<HashMap<PeerId, KnownPeer>>,
    dirty: AtomicBool,
}

impl KnownPeers {
    pub fn get(&self, id: &PeerId) -> Option<KnownPeer> {
        self.peers.lock().unwrap().get(id).cloned()
    }

    pub fn all(&self) -> Vec<(PeerId, KnownPeer)> {
        self.peers
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| (*id, p.clone()))
            .collect()
    }

    /// Record (or refresh) a peer. Keys never change for a PeerId, since the
    /// PeerId is the hash of the signing key.
    pub fn remember(&self, id: PeerId, peer: KnownPeer) {
        let mut peers = self.peers.lock().unwrap();
        let changed = peers
            .get(&id)
            .is_none_or(|p| p.name != peer.name || peer.last_seen >= p.last_seen + 60);
        peers.insert(id, peer);
        if peers.len() > MAX_KNOWN_PEERS
            && let Some(oldest) = peers
                .iter()
                .min_by_key(|(_, p)| p.last_seen)
                .map(|(id, _)| *id)
        {
            peers.remove(&oldest);
        }
        if changed {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    fn load(&self, path: &Path) {
        if let Some(map) = read_json::<HashMap<String, KnownPeer>>(path) {
            let mut peers = self.peers.lock().unwrap();
            for (id, peer) in map {
                if let Some(id) = PeerId::from_hex(&id)
                    && id == PeerId::from_public_key(&peer.ed25519)
                {
                    peers.insert(id, peer);
                }
            }
        }
    }

    fn save(&self, path: &Path) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let map: HashMap<String, KnownPeer> = self
            .peers
            .lock()
            .unwrap()
            .iter()
            .map(|(id, p)| (id.to_string(), p.clone()))
            .collect();
        write_json(path, &map);
    }
}

/// A packet held for a peer that is out of range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stored {
    pub dest: PeerId,
    pub source: PeerId,
    #[serde(with = "hex_id")]
    pub message_id: [u8; 8],
    /// Unix seconds when we took custody.
    pub stored_at: u32,
    /// Encoded [`amara_protocol::Envelope`].
    #[serde(with = "hex_bytes")]
    pub envelope: Vec<u8>,
}

/// Bounded FIFO of packets waiting for their destination to come back.
#[derive(Default)]
pub struct Custody {
    items: Mutex<VecDeque<Stored>>,
    dirty: AtomicBool,
}

impl Custody {
    /// Hold a packet. Returns false if it is already held.
    pub fn hold(&self, item: Stored) -> bool {
        let mut items = self.items.lock().unwrap();
        if items
            .iter()
            .any(|s| s.source == item.source && s.message_id == item.message_id)
        {
            return false;
        }
        let per_peer = items.iter().filter(|s| s.dest == item.dest).count();
        if per_peer >= MAX_STORED_PER_PEER
            && let Some(pos) = items.iter().position(|s| s.dest == item.dest)
        {
            items.remove(pos);
        }
        if items.len() >= MAX_STORED_MESSAGES {
            items.pop_front();
        }
        items.push_back(item);
        self.dirty.store(true, Ordering::Relaxed);
        true
    }

    /// Remove and return everything held for `dest`.
    pub fn take_for(&self, dest: &PeerId) -> Vec<Stored> {
        let mut items = self.items.lock().unwrap();
        let (out, keep): (Vec<_>, Vec<_>) = items.drain(..).partition(|s| s.dest == *dest);
        *items = keep.into();
        if !out.is_empty() {
            self.dirty.store(true, Ordering::Relaxed);
        }
        out
    }

    /// Drop a held packet (e.g. once it was delivered some other way).
    pub fn forget(&self, source: &PeerId, message_id: &[u8; 8]) {
        let mut items = self.items.lock().unwrap();
        let before = items.len();
        items.retain(|s| !(s.source == *source && s.message_id == *message_id));
        if items.len() != before {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    pub fn len(&self) -> usize {
        self.items.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Destinations with packets waiting, and how many each.
    pub fn summary(&self) -> Vec<(PeerId, usize)> {
        let mut counts: HashMap<PeerId, usize> = HashMap::new();
        for s in self.items.lock().unwrap().iter() {
            *counts.entry(s.dest).or_default() += 1;
        }
        counts.into_iter().collect()
    }

    /// Drop packets older than the store-and-forward TTL.
    pub fn expire(&self) {
        let now = now_timestamp();
        let mut items = self.items.lock().unwrap();
        let before = items.len();
        items.retain(|s| now.saturating_sub(s.stored_at) as u64 <= STORE_FORWARD_TTL_SECS);
        if items.len() != before {
            self.dirty.store(true, Ordering::Relaxed);
        }
    }

    fn load(&self, path: &Path) {
        if let Some(list) = read_json::<VecDeque<Stored>>(path) {
            *self.items.lock().unwrap() = list;
            self.expire();
        }
    }

    fn save(&self, path: &Path) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let items = self.items.lock().unwrap().clone();
        write_json(path, &items);
    }
}

/// On-disk locations of the store-and-forward state.
pub struct StatePaths {
    known: PathBuf,
    custody: PathBuf,
}

impl StatePaths {
    pub fn new(dir: &Path) -> Self {
        Self {
            known: dir.join("known_peers.json"),
            custody: dir.join("custody.json"),
        }
    }

    pub fn load(&self, known: &KnownPeers, custody: &Custody) {
        known.load(&self.known);
        custody.load(&self.custody);
    }

    pub fn save(&self, known: &KnownPeers, custody: &Custody) {
        known.save(&self.known);
        custody.save(&self.custody);
    }
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text)
        .map_err(|e| warn!("ignoring corrupt {}: {e}", path.display()))
        .ok()
}

fn write_json<T: Serialize>(path: &Path, value: &T) {
    let result = (|| -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_vec(value)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        }
        std::fs::rename(tmp, path)
    })();
    if let Err(e) = result {
        warn!("cannot save {}: {e}", path.display());
    }
}

mod hex_key {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(key: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(key))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        let bytes = hex::decode(String::deserialize(d)?).map_err(D::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| D::Error::custom("expected 32 bytes"))
    }
}

mod hex_id {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(id: &[u8; 8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(id))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 8], D::Error> {
        let bytes = hex::decode(String::deserialize(d)?).map_err(D::Error::custom)?;
        bytes
            .try_into()
            .map_err(|_| D::Error::custom("expected 8 bytes"))
    }
}

mod hex_bytes {
    use serde::{Deserialize, Deserializer, Serializer, de::Error};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        hex::decode(String::deserialize(d)?).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(dest: u8, n: u8) -> Stored {
        Stored {
            dest: PeerId::from_bytes([dest; 8]),
            source: PeerId::from_bytes([1; 8]),
            message_id: [n; 8],
            stored_at: now_timestamp(),
            envelope: vec![n],
        }
    }

    #[test]
    fn custody_bounds_and_dedups() {
        let c = Custody::default();
        assert!(c.hold(stored(2, 0)));
        assert!(!c.hold(stored(2, 0)), "duplicate");
        for n in 1..=(MAX_STORED_PER_PEER as u8) {
            c.hold(stored(2, n));
        }
        assert_eq!(c.len(), MAX_STORED_PER_PEER, "per-peer cap evicts oldest");
        c.hold(stored(3, 0));
        let taken = c.take_for(&PeerId::from_bytes([2; 8]));
        assert_eq!(taken.len(), MAX_STORED_PER_PEER);
        assert_eq!(taken[0].message_id, [1; 8]);
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn expiry_drops_old_packets() {
        let c = Custody::default();
        let mut old = stored(2, 0);
        old.stored_at = now_timestamp() - STORE_FORWARD_TTL_SECS as u32 - 10;
        c.hold(old);
        c.hold(stored(2, 1));
        c.expire();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn state_survives_restart() {
        let dir = std::env::temp_dir().join(format!("amara-store-{}", rand::random::<u64>()));
        let paths = StatePaths::new(&dir);
        let (known, custody) = (KnownPeers::default(), Custody::default());
        let peer = KnownPeer {
            ed25519: [7; 32],
            x25519: [8; 32],
            name: "nova".into(),
            last_seen: 5,
        };
        let id = PeerId::from_public_key(&peer.ed25519);
        known.remember(id, peer.clone());
        known.remember(PeerId::from_bytes([1; 8]), peer.clone()); // id does not match key
        custody.hold(stored(2, 9));
        paths.save(&known, &custody);

        let (known2, custody2) = (KnownPeers::default(), Custody::default());
        paths.load(&known2, &custody2);
        assert_eq!(known2.get(&id), Some(peer));
        assert_eq!(known2.all().len(), 1, "forged entries are dropped on load");
        assert_eq!(custody2.take_for(&PeerId::from_bytes([2; 8])).len(), 1);
        let _ = std::fs::remove_dir_all(dir);
    }
}
