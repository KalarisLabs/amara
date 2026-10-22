//! Live topics: the pub/sub layer robots, sensors and agents share.
//!
//! A sample is a signed broadcast (`EVENT`) that travels a few hops, so a
//! robot's telemetry reaches whoever is physically nearby without any broker.
//! Every node keeps the newest value per (sender, topic), so an agent that
//! arrives late can read the current state immediately instead of waiting for
//! the next update. Streaming calls cover the other robotics pattern: a robot
//! exposes `move_to` as a tool and streams progress back (like a ROS action).
//!
//! The reserved topic [`ESTOP_TOPIC`] is the mesh-wide emergency stop.

use crate::inbox::InboxItem;
use crate::runtime::AgentRuntime;
use amara_core::constants::MAX_TTL;
use amara_core::peer::{PeerId, TrustLevel};
use amara_core::types::now_timestamp;
use amara_core::{AmaraError, Result};
use amara_mesh::Inbound;
use amara_protocol::messages::encode_body;
use amara_protocol::{Packet, PacketFlags, PacketType, Sample};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Reserved topic for emergency stops.
pub const ESTOP_TOPIC: &str = "amara/estop";
/// Default reach of a sample, in hops.
pub const DEFAULT_TOPIC_HOPS: u8 = 2;
/// Largest encoded sample body.
pub const MAX_SAMPLE_BYTES: usize = 16 * 1024;
/// Distinct (sender, topic) pairs remembered.
const MAX_CACHED: usize = 1024;

/// The newest value seen on a topic from one sender.
#[derive(Debug, Clone, Serialize)]
pub struct TopicSample {
    pub topic: String,
    pub from: String,
    pub from_name: String,
    pub seq: u64,
    pub data: Value,
    /// Sender's clock, unix seconds.
    pub at: u32,
    /// Milliseconds since this node received it (filled in when listed).
    pub age_ms: u64,
    #[serde(skip)]
    received: Option<Instant>,
}

/// Who may trigger this node's emergency stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EstopFrom {
    Off,
    Trusted,
    /// Stopping is fail-safe, so by default anyone in range may do it.
    #[default]
    Anyone,
}

/// Emergency-stop behaviour for nodes that drive hardware.
#[derive(Debug, Clone, Default)]
pub struct EstopPolicy {
    pub from: EstopFrom,
    /// Program (and fixed args) run on every accepted stop; the stop is written
    /// to its stdin as JSON. No shell is involved.
    pub command: Option<(String, Vec<String>)>,
}

/// Validate a topic name: `[a-z0-9_.-/]{1,128}`, no leading `/`, no `amara/` prefix.
pub fn validate_topic(topic: &str) -> std::result::Result<(), String> {
    if topic.is_empty() || topic.len() > 128 {
        return Err("topic must be 1-128 characters".into());
    }
    if !topic
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_.-/".contains(c))
    {
        return Err("topic may only contain a-z 0-9 _ . - /".into());
    }
    if topic.starts_with('/') || topic.ends_with('/') || topic.contains("//") {
        return Err("topic segments must not be empty".into());
    }
    if topic.starts_with("amara/") {
        return Err("the amara/ prefix is reserved".into());
    }
    Ok(())
}

/// `pattern` matches a topic exactly, or as a prefix ending in `/*` (`rover1/*`),
/// or everything (`*`).
pub fn topic_matches(pattern: &str, topic: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some("") => true,
        Some(prefix) => topic.starts_with(prefix),
        None => pattern == topic,
    }
}

#[derive(Default)]
struct Board {
    latest: HashMap<(PeerId, String), TopicSample>,
    own_seq: HashMap<String, u64>,
}

/// Topic state kept by every runtime.
pub(crate) struct Topics {
    board: Mutex<Board>,
    feed: broadcast::Sender<TopicSample>,
}

impl Default for Topics {
    fn default() -> Self {
        Self {
            board: Mutex::new(Board::default()),
            feed: broadcast::channel(4096).0,
        }
    }
}

impl Topics {
    /// Record a sample and pass it to subscribers. Relays add jitter, so samples
    /// can arrive out of order: subscribers see every one (the mesh already
    /// dropped duplicates), while the cache only moves forward. Returns whether
    /// the sample became the latest value.
    fn record(&self, from: PeerId, sample: TopicSample) -> bool {
        let _ = self.feed.send(sample.clone());
        let mut board = self.board.lock().unwrap();
        let key = (from, sample.topic.clone());
        if let Some(prev) = board.latest.get(&key) {
            let recent = prev
                .received
                .is_some_and(|r| r.elapsed() < Duration::from_secs(10));
            // A lower seq after a quiet spell means the sender restarted.
            if sample.seq <= prev.seq && recent {
                return false;
            }
        }
        if board.latest.len() >= MAX_CACHED
            && !board.latest.contains_key(&key)
            && let Some(oldest) = board
                .latest
                .iter()
                .min_by_key(|(_, s)| s.received)
                .map(|(k, _)| k.clone())
        {
            board.latest.remove(&oldest);
        }
        board.latest.insert(key, sample);
        true
    }
}

impl AgentRuntime {
    /// Publish `data` on `topic` to everyone within `hops` (default 2). Returns the sequence number.
    pub async fn publish(&self, topic: &str, data: Value, hops: Option<u8>) -> Result<u64> {
        validate_topic(topic).map_err(AmaraError::Config)?;
        self.publish_raw(topic, data, hops.unwrap_or(DEFAULT_TOPIC_HOPS))
            .await
    }

    async fn publish_raw(&self, topic: &str, data: Value, hops: u8) -> Result<u64> {
        let seq = {
            let mut board = self.topics.board.lock().unwrap();
            let seq = board.own_seq.entry(topic.to_string()).or_insert(0);
            *seq += 1;
            *seq
        };
        let sample = Sample {
            topic: topic.to_string(),
            seq,
            data,
        };
        let (bytes, compressed) = encode_body(&sample)?;
        if bytes.len() > MAX_SAMPLE_BYTES {
            return Err(AmaraError::PacketTooLarge {
                size: bytes.len(),
                max: MAX_SAMPLE_BYTES,
            });
        }
        let me = self.identity().peer_id();
        // `hops` counts links travelled; TTL counts relays after the first link.
        let ttl = hops.clamp(1, MAX_TTL + 1) - 1;
        let mut packet = Packet::broadcast(PacketType::Event, me, ttl, bytes);
        if compressed {
            packet.header.flags.set(PacketFlags::COMPRESSED);
        }
        self.mesh.send_packet(packet).await?;
        self.topics.record(
            me,
            TopicSample {
                topic: sample.topic,
                from: me.to_string(),
                from_name: self.identity().display_name().to_string(),
                seq,
                data: sample.data,
                at: now_timestamp(),
                age_ms: 0,
                received: Some(Instant::now()),
            },
        );
        Ok(seq)
    }

    /// Newest value per (sender, topic), optionally filtered by a pattern
    /// (`rover1/battery`, `rover1/*`, `*`).
    pub fn topics(&self, pattern: Option<&str>) -> Vec<TopicSample> {
        let board = self.topics.board.lock().unwrap();
        let mut out: Vec<TopicSample> = board
            .latest
            .values()
            .filter(|s| pattern.is_none_or(|p| topic_matches(p, &s.topic)))
            .map(|s| {
                let mut s = s.clone();
                s.age_ms = s
                    .received
                    .map(|r| r.elapsed().as_millis() as u64)
                    .unwrap_or(0);
                s
            })
            .collect();
        out.sort_by(|a, b| {
            a.topic
                .cmp(&b.topic)
                .then_with(|| a.from_name.cmp(&b.from_name))
        });
        out
    }

    /// Every sample as it arrives (including this node's own and emergency stops).
    pub fn subscribe_topics(&self) -> broadcast::Receiver<TopicSample> {
        self.topics.feed.subscribe()
    }

    /// Emergency stop: every robot in range (or only `target`) halts. Travels the
    /// full mesh radius. Robots decide whether to honour it ([`EstopPolicy`]).
    pub async fn estop(&self, target: Option<PeerId>, reason: &str) -> Result<()> {
        let data = json!({
            "target": target.map(|t| t.to_string()),
            "reason": reason,
        });
        self.publish_raw(ESTOP_TOPIC, data, MAX_TTL + 1)
            .await
            .map(|_| ())
    }

    pub(crate) fn handle_sample(&self, inbound: Inbound) -> Result<()> {
        let packet = &inbound.packet;
        let from = packet.header.source_id;
        let sample: Sample = self.open_body(packet)?;
        if sample.topic != ESTOP_TOPIC {
            validate_topic(&sample.topic).map_err(AmaraError::MalformedPacket)?;
        }
        self.topics.record(
            from,
            TopicSample {
                topic: sample.topic.clone(),
                from: from.to_string(),
                from_name: inbound.sender_name.clone(),
                seq: sample.seq,
                data: sample.data.clone(),
                at: packet.header.timestamp,
                age_ms: 0,
                received: Some(Instant::now()),
            },
        );
        // Every stop counts, even one overtaken by a later one.
        if sample.topic == ESTOP_TOPIC {
            self.handle_estop(from, &inbound.sender_name, &sample.data);
        }
        Ok(())
    }

    fn handle_estop(&self, from: PeerId, from_name: &str, data: &Value) {
        let me = self.identity().peer_id().to_string();
        if let Some(target) = data.get("target").and_then(Value::as_str)
            && target != me
        {
            return;
        }
        let policy = &self.config.estop;
        let trust = self
            .mesh
            .registry()
            .get_peer(&from)
            .map(|p| p.trust_level)
            .unwrap_or_default();
        let allowed = match policy.from {
            EstopFrom::Off => false,
            EstopFrom::Trusted => trust >= TrustLevel::Trusted,
            EstopFrom::Anyone => trust != TrustLevel::Blocked,
        };
        let reason = data
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if !allowed {
            debug!("ignored emergency stop from {from_name} ({from}) by policy");
            return;
        }
        warn!("EMERGENCY STOP from {from_name} ({from}): {reason}");
        self.inbox.push(InboxItem::Stop {
            from: from.to_string(),
            from_name: from_name.to_string(),
            reason: reason.clone(),
            at: now_timestamp(),
        });
        if let Some((program, args)) = policy.command.clone() {
            let input = json!({"from": from.to_string(), "from_name": from_name, "reason": reason});
            tokio::spawn(async move {
                let child = tokio::process::Command::new(&program)
                    .args(&args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .spawn();
                match child {
                    Ok(mut child) => {
                        if let Some(mut stdin) = child.stdin.take() {
                            let _ = stdin.write_all(input.to_string().as_bytes()).await;
                        }
                        let _ = child.wait().await;
                    }
                    Err(e) => warn!("emergency stop command {program} failed to start: {e}"),
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_rules() {
        assert!(validate_topic("rover1/battery").is_ok());
        assert!(validate_topic("lab/temp.c").is_ok());
        for bad in ["", "/x", "x/", "a//b", "Upper", "amara/estop", "sp ace"] {
            assert!(validate_topic(bad).is_err(), "{bad}");
        }
        assert!(topic_matches("*", "a/b"));
        assert!(topic_matches("rover1/*", "rover1/battery"));
        assert!(!topic_matches("rover1/*", "rover2/battery"));
        assert!(topic_matches("a/b", "a/b") && !topic_matches("a/b", "a/bc"));
    }

    fn sample(topic: &str, seq: u64) -> TopicSample {
        TopicSample {
            topic: topic.into(),
            from: String::new(),
            from_name: String::new(),
            seq,
            data: Value::Null,
            at: 0,
            age_ms: 0,
            received: Some(Instant::now()),
        }
    }

    #[test]
    fn board_keeps_newest() {
        let t = Topics::default();
        let p = PeerId::from_bytes([1; 8]);
        assert!(t.record(p, sample("a", 5)));
        let mut feed = t.feed.subscribe();
        assert!(
            !t.record(p, sample("a", 4)),
            "stale values do not replace newer ones"
        );
        assert_eq!(
            feed.try_recv().unwrap().seq,
            4,
            "but subscribers still see them"
        );
        assert!(!t.record(p, sample("a", 5)), "repeat");
        assert!(t.record(p, sample("a", 6)));
        assert!(t.record(p, sample("b", 1)), "other topics are independent");
        assert!(
            t.record(PeerId::from_bytes([2; 8]), sample("a", 1)),
            "other senders too"
        );
    }
}
