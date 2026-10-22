//! File transfer between agents.
//!
//! ```text
//! sender                                        receiver
//!   │ FILE_MANIFEST sealed(FileOffer) ──────────►│ policy: accept? size? trusted?
//!   │◄────────────── FILE_COMPLETE {accepted | rejected}
//!   │ FILE_CHUNK sealed(id · index · bytes) × n ─►│ written straight to a .part file
//!   │ FILE_COMPLETE {sent} ──────────────────────►│ which chunks are missing?
//!   │◄────────────── FILE_COMPLETE {missing: [..]}  (resend those, ask again)
//!   │◄────────────── FILE_COMPLETE {received}       hash verified, file saved
//! ```
//!
//! Chunks are paced so a transfer stays well under every relay's per-source rate
//! limit, and losses on lossy links (UDP, BLE) are repaired by the receiver
//! listing what it is missing. Files never touch memory as a whole on either side.

use crate::inbox::InboxItem;
use crate::runtime::AgentRuntime;
use amara_core::peer::{PeerId, TrustLevel};
use amara_core::types::now_timestamp;
use amara_core::{AmaraError, Result};
use amara_mesh::Inbound;
use amara_protocol::{FileChunk, FileOffer, FileState, FileStatus, PacketType};
use serde::{Deserialize, Serialize};
use std::io::SeekFrom;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::{debug, info};
use uuid::Uuid;

/// Bytes per FILE_CHUNK packet.
pub const FILE_CHUNK_SIZE: u32 = 32 * 1024;
/// Largest chunk size a receiver accepts from a sender.
const MAX_CHUNK_SIZE: u32 = 256 * 1024;
/// Gap between chunk packets (50/s, a quarter of the relays' per-source packet budget).
const CHUNK_PACING: Duration = Duration::from_millis(20);
/// Simultaneous incoming transfers.
const MAX_INCOMING: usize = 8;
/// Most missing chunk indices reported in one status.
const MAX_MISSING_REPORTED: usize = 4096;
/// Repair rounds before a transfer is abandoned.
const MAX_ROUNDS: u32 = 10;
const ANSWER_TIMEOUT: Duration = Duration::from_secs(20);
/// Incoming transfers with no traffic for this long are dropped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Who may send this agent files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AcceptFiles {
    Off,
    Trusted,
    #[default]
    Anyone,
}

/// Incoming file policy.
#[derive(Debug, Clone)]
pub struct FilePolicy {
    pub accept: AcceptFiles,
    /// Largest file accepted, in bytes.
    pub max_size: u64,
    /// Where received files are saved. `None` refuses all files.
    pub dir: Option<PathBuf>,
}

impl Default for FilePolicy {
    fn default() -> Self {
        Self {
            accept: AcceptFiles::Anyone,
            max_size: 32 * 1024 * 1024,
            dir: None,
        }
    }
}

/// Outcome of a completed send.
#[derive(Debug, Clone, Serialize)]
pub struct FileReceipt {
    pub id: Uuid,
    pub name: String,
    pub size: u64,
    pub hash: String,
    pub ms: u64,
    /// Chunks that had to be sent more than once.
    pub resent: u32,
}

pub(crate) struct IncomingFile {
    from: PeerId,
    from_name: String,
    offer: FileOffer,
    part: PathBuf,
    received: Vec<bool>,
    last: Instant,
}

impl IncomingFile {
    fn missing(&self) -> Vec<u32> {
        self.received
            .iter()
            .enumerate()
            .filter(|(_, got)| !**got)
            .map(|(i, _)| i as u32)
            .take(MAX_MISSING_REPORTED)
            .collect()
    }
}

enum Source {
    Path(PathBuf),
    Bytes(Arc<Vec<u8>>),
}

impl Source {
    async fn read(&self, offset: u64, len: usize) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Bytes(b) => Ok(b[offset as usize..offset as usize + len].to_vec()),
            Self::Path(p) => {
                let mut f = tokio::fs::File::open(p).await?;
                f.seek(SeekFrom::Start(offset)).await?;
                let mut buf = vec![0; len];
                f.read_exact(&mut buf).await?;
                Ok(buf)
            }
        }
    }
}

/// Keep a received file name harmless: no directories, no hidden files, no
/// control or reserved characters.
pub fn sanitize_file_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = base
        .chars()
        .filter(|c| !c.is_control() && !"<>:\"|?*".contains(*c))
        .take(120)
        .collect();
    let clean = clean.trim().trim_start_matches('.').trim().to_string();
    if clean.is_empty() {
        "file".into()
    } else {
        clean
    }
}

/// `dir/name`, or `dir/name (n).ext` if that is taken.
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

async fn hash_file(path: PathBuf) -> Result<String> {
    tokio::task::spawn_blocking(move || -> std::io::Result<String> {
        let mut hasher = blake3::Hasher::new();
        std::io::copy(&mut std::fs::File::open(path)?, &mut hasher)?;
        Ok(hasher.finalize().to_hex().to_string())
    })
    .await
    .map_err(|e| AmaraError::Other(e.to_string()))?
    .map_err(Into::into)
}

impl AgentRuntime {
    /// Send a file from disk to a peer in range. Returns once the receiver has
    /// verified it.
    pub async fn send_file(&self, to: PeerId, path: &Path) -> Result<FileReceipt> {
        let meta = tokio::fs::metadata(path).await?;
        if !meta.is_file() {
            return Err(AmaraError::Other(format!(
                "{} is not a file",
                path.display()
            )));
        }
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into());
        let hash = hash_file(path.to_path_buf()).await?;
        self.transfer(to, name, meta.len(), hash, Source::Path(path.to_path_buf()))
            .await
    }

    /// Send in-memory bytes as a file named `name`.
    pub async fn send_file_bytes(
        &self,
        to: PeerId,
        name: &str,
        data: Vec<u8>,
    ) -> Result<FileReceipt> {
        let hash = blake3::hash(&data).to_hex().to_string();
        let size = data.len() as u64;
        self.transfer(
            to,
            name.to_string(),
            size,
            hash,
            Source::Bytes(Arc::new(data)),
        )
        .await
    }

    async fn transfer(
        &self,
        to: PeerId,
        name: String,
        size: u64,
        hash: String,
        source: Source,
    ) -> Result<FileReceipt> {
        let peer = self.mesh.registry().get_peer(&to).ok_or_else(|| {
            AmaraError::UnknownPeer(format!("{to} is not in range (files are not stored)"))
        })?;
        let started = Instant::now();
        let offer = FileOffer {
            id: Uuid::new_v4(),
            name,
            size,
            chunk: FILE_CHUNK_SIZE,
            hash,
            mime: None,
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.files_out.insert(offer.id, (to, tx));
        let result = self.run_transfer(to, &offer, &source, &mut rx).await;
        self.files_out.remove(&offer.id);
        let resent = result?;
        info!(
            "sent {} ({} bytes) to {} in {}ms",
            offer.name,
            offer.size,
            peer.display_name,
            started.elapsed().as_millis()
        );
        Ok(FileReceipt {
            id: offer.id,
            name: offer.name,
            size: offer.size,
            hash: offer.hash,
            ms: started.elapsed().as_millis() as u64,
            resent,
        })
    }

    async fn run_transfer(
        &self,
        to: PeerId,
        offer: &FileOffer,
        source: &Source,
        rx: &mut mpsc::UnboundedReceiver<FileStatus>,
    ) -> Result<u32> {
        self.send_body(PacketType::FileManifest, to, offer).await?;
        match tokio::time::timeout(ANSWER_TIMEOUT, rx.recv()).await {
            Ok(Some(s)) if s.state == FileState::Accepted => {}
            Ok(Some(s)) if s.state == FileState::Rejected => {
                return Err(AmaraError::PermissionDenied(
                    s.error.unwrap_or_else(|| "file rejected".into()),
                ));
            }
            Ok(Some(s)) => {
                return Err(AmaraError::Other(format!(
                    "unexpected answer {:?}",
                    s.state
                )));
            }
            _ => {
                return Err(AmaraError::Timeout(
                    "the receiver did not answer the file offer".into(),
                ));
            }
        }

        let chunks = offer.chunks();
        let mut todo: Vec<u32> = (0..chunks).collect();
        let mut resent = 0u32;
        for round in 0..=MAX_ROUNDS {
            for &index in &todo {
                let offset = index as u64 * offer.chunk as u64;
                let len = (offer.size - offset).min(offer.chunk as u64) as usize;
                let chunk = FileChunk {
                    id: offer.id,
                    index,
                    data: source.read(offset, len).await?,
                };
                let packet = self.seal_packet(PacketType::FileChunk, to, chunk.encode(), false)?;
                self.mesh.send_packet(packet).await?;
                tokio::time::sleep(CHUNK_PACING).await;
            }
            if round > 0 {
                resent += todo.len() as u32;
            }
            let status = self.ask_status(to, offer.id, rx).await?;
            match status.state {
                FileState::Received => return Ok(resent),
                FileState::Rejected => {
                    return Err(AmaraError::Other(
                        status.error.unwrap_or_else(|| "file rejected".into()),
                    ));
                }
                FileState::Missing => {
                    todo = status.missing.into_iter().filter(|i| *i < chunks).collect();
                    debug!("{}: resending {} chunk(s)", offer.name, todo.len());
                }
                _ => todo.clear(),
            }
        }
        Err(AmaraError::Other(format!(
            "gave up on {} after {MAX_ROUNDS} repair rounds",
            offer.name
        )))
    }

    /// Tell the receiver every chunk went out once; wait for its verdict.
    async fn ask_status(
        &self,
        to: PeerId,
        id: Uuid,
        rx: &mut mpsc::UnboundedReceiver<FileStatus>,
    ) -> Result<FileStatus> {
        let sent = FileStatus {
            id,
            state: FileState::Sent,
            missing: Vec::new(),
            error: None,
        };
        for _ in 0..3 {
            self.send_body(PacketType::FileComplete, to, &sent).await?;
            let deadline = tokio::time::Instant::now() + ANSWER_TIMEOUT;
            while let Ok(Some(status)) = tokio::time::timeout_at(deadline, rx.recv()).await {
                if status.state != FileState::Accepted {
                    return Ok(status);
                }
            }
        }
        Err(AmaraError::Timeout("the receiver stopped answering".into()))
    }

    async fn send_status(
        &self,
        to: PeerId,
        id: Uuid,
        state: FileState,
        missing: Vec<u32>,
        error: Option<String>,
    ) -> Result<()> {
        let status = FileStatus {
            id,
            state,
            missing,
            error,
        };
        self.send_body(PacketType::FileComplete, to, &status).await
    }

    pub(crate) async fn handle_file_packet(&self, inbound: &Inbound) -> Result<()> {
        let packet = &inbound.packet;
        let from = packet.header.source_id;
        if packet.header.destination_id != self.identity().peer_id() {
            return Ok(());
        }
        match packet.header.packet_type {
            PacketType::FileManifest => {
                let offer: FileOffer = self.open_body(packet)?;
                let id = offer.id;
                match self.accept_offer(from, &inbound.sender_name, offer).await {
                    Ok(()) => {
                        self.send_status(from, id, FileState::Accepted, vec![], None)
                            .await
                    }
                    Err(why) => {
                        self.send_status(from, id, FileState::Rejected, vec![], Some(why))
                            .await
                    }
                }
            }
            PacketType::FileChunk => {
                let chunk = FileChunk::decode(&self.open_raw(packet)?)?;
                self.write_chunk(from, chunk).await
            }
            PacketType::FileComplete => {
                let status: FileStatus = self.open_body(packet)?;
                if status.state == FileState::Sent {
                    return self.finish_incoming(from, status.id).await;
                }
                if let Some(entry) = self.files_out.get(&status.id)
                    && entry.0 == from
                {
                    let _ = entry.1.send(status);
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn accept_offer(
        &self,
        from: PeerId,
        from_name: &str,
        offer: FileOffer,
    ) -> std::result::Result<(), String> {
        let policy = &self.config.files;
        let dir = match (&policy.dir, policy.accept) {
            (Some(dir), accept) if accept != AcceptFiles::Off => dir,
            _ => return Err("this agent does not accept files".into()),
        };
        let trust = self
            .mesh
            .registry()
            .get_peer(&from)
            .map(|p| p.trust_level)
            .unwrap_or_default();
        if trust == TrustLevel::Blocked
            || (policy.accept == AcceptFiles::Trusted && trust < TrustLevel::Trusted)
        {
            return Err("this agent only accepts files from trusted peers".into());
        }
        if offer.size > policy.max_size {
            return Err(format!(
                "file too large ({} bytes, limit {})",
                offer.size, policy.max_size
            ));
        }
        if offer.chunk == 0 || offer.chunk > MAX_CHUNK_SIZE {
            return Err("invalid chunk size".into());
        }
        if offer.hash.len() != 64 || !offer.hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err("invalid hash".into());
        }
        if self.files_in.contains_key(&offer.id) {
            return Ok(()); // repeated offer
        }
        if self.files_in.len() >= MAX_INCOMING {
            return Err("busy: too many incoming transfers".into());
        }
        let part = dir.join(format!(".{}.part", offer.id));
        let created = async {
            tokio::fs::create_dir_all(dir).await?;
            tokio::fs::File::create(&part)
                .await?
                .set_len(offer.size)
                .await
        };
        created
            .await
            .map_err(|e| format!("cannot store file: {e}"))?;
        info!(
            "receiving {} ({} bytes) from {from_name}",
            offer.name, offer.size
        );
        self.files_in.insert(
            offer.id,
            IncomingFile {
                from,
                from_name: from_name.to_string(),
                received: vec![false; offer.chunks() as usize],
                offer,
                part,
                last: Instant::now(),
            },
        );
        Ok(())
    }

    async fn write_chunk(&self, from: PeerId, chunk: FileChunk) -> Result<()> {
        let (part, offset) = {
            let Some(mut entry) = self.files_in.get_mut(&chunk.id) else {
                return Ok(());
            };
            let f = entry.value_mut();
            let offer = &f.offer;
            let offset = chunk.index as u64 * offer.chunk as u64;
            let expected = offer.size.saturating_sub(offset).min(offer.chunk as u64);
            if f.from != from
                || chunk.index >= offer.chunks()
                || chunk.data.len() as u64 != expected
            {
                return Err(AmaraError::MalformedPacket("bad file chunk".into()));
            }
            f.last = Instant::now();
            if f.received[chunk.index as usize] {
                return Ok(());
            }
            (f.part.clone(), offset)
        };
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .await?;
        file.seek(SeekFrom::Start(offset)).await?;
        file.write_all(&chunk.data).await?;
        if let Some(mut entry) = self.files_in.get_mut(&chunk.id) {
            entry.received[chunk.index as usize] = true;
        }
        Ok(())
    }

    async fn finish_incoming(&self, from: PeerId, id: Uuid) -> Result<()> {
        if let Some(done) = self.files_done.get(&id)
            && done.0 == from
        {
            // Our verdict was lost; repeat it.
            return self
                .send_status(from, id, FileState::Received, vec![], None)
                .await;
        }
        let missing = match self.files_in.get(&id) {
            Some(f) if f.from == from => f.missing(),
            _ => return Ok(()),
        };
        if !missing.is_empty() {
            return self
                .send_status(from, id, FileState::Missing, missing, None)
                .await;
        }
        let Some((_, incoming)) = self.files_in.remove(&id) else {
            return Ok(());
        };
        let hash = hash_file(incoming.part.clone()).await?;
        if !hash.eq_ignore_ascii_case(&incoming.offer.hash) {
            let _ = tokio::fs::remove_file(&incoming.part).await;
            return self
                .send_status(
                    from,
                    id,
                    FileState::Rejected,
                    vec![],
                    Some("hash mismatch".into()),
                )
                .await;
        }
        let dir = incoming.part.parent().unwrap_or(Path::new("."));
        let path = unique_path(dir, &sanitize_file_name(&incoming.offer.name));
        tokio::fs::rename(&incoming.part, &path).await?;
        self.files_done.insert(id, (from, Instant::now()));
        info!("received {} from {}", path.display(), incoming.from_name);
        self.inbox.push(InboxItem::File {
            from: from.to_string(),
            from_name: incoming.from_name,
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            path: path.display().to_string(),
            size: incoming.offer.size,
            hash,
            at: now_timestamp(),
        });
        self.send_status(from, id, FileState::Received, vec![], None)
            .await
    }

    /// Drop stalled incoming transfers and old completion records.
    pub(crate) async fn file_janitor(self: Arc<Self>) {
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let stale: Vec<Uuid> = self
                .files_in
                .iter()
                .filter(|f| f.last.elapsed() > IDLE_TIMEOUT)
                .map(|f| *f.key())
                .collect();
            for id in stale {
                if let Some((_, f)) = self.files_in.remove(&id) {
                    debug!("dropping stalled transfer {}", f.offer.name);
                    let _ = tokio::fs::remove_file(&f.part).await;
                }
            }
            self.files_done
                .retain(|_, (_, at)| at.elapsed() < Duration::from_secs(600));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_sanitized() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("C:\\Windows\\evil.exe"), "evil.exe");
        assert_eq!(sanitize_file_name(".bashrc"), "bashrc");
        assert_eq!(sanitize_file_name("a<b>:c?.txt"), "abc.txt");
        assert_eq!(sanitize_file_name(""), "file");
        assert_eq!(sanitize_file_name("..."), "file");
        assert_eq!(sanitize_file_name("report final.pdf"), "report final.pdf");
    }

    #[test]
    fn unique_paths_do_not_overwrite() {
        let dir = std::env::temp_dir().join(format!("amara-files-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a.txt"));
        std::fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (1).txt"));
        std::fs::write(dir.join("noext"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "noext"), dir.join("noext (1)"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
