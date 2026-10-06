use crate::handler::{TaskContext, TaskHandler};
use amara_core::capability::{Availability, CapabilityDocument, LoadClass, ResourceLimits};
use amara_core::task::{Task, TaskResult, TaskResultStatus};
use amara_core::{AmaraError, Result};
use amara_crypto::identity::NodeIdentity;
use amara_mesh::MeshEngine;
use amara_protocol::codec::{decode_task, encode_announce, encode_capability, encode_result, encode_task};
use amara_protocol::{Packet, PacketType};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, error, info, warn};
use tokio::time::Instant;

/// The agent runtime manages task execution and mesh integration.
pub struct AgentRuntime {
    mesh: Arc<MeshEngine>,
    handlers: DashMap<String, Box<dyn TaskHandler>>,
    pending_tasks: DashMap<amara_core::task::TaskId, tokio::sync::oneshot::Sender<TaskResult>>,
}

impl AgentRuntime {
    pub fn new(mesh: Arc<MeshEngine>) -> Arc<Self> {
        Arc::new(Self {
            mesh,
            handlers: DashMap::new(),
            pending_tasks: DashMap::new(),
        })
    }

    pub fn identity(&self) -> &NodeIdentity {
        self.mesh.identity()
    }
    
    pub fn mesh(&self) -> &Arc<MeshEngine> {
        &self.mesh
    }

    /// Register a new task handler for a specific capability.
    pub fn register_handler(&self, handler: Box<dyn TaskHandler>) {
        let cap = handler.capability().to_string();
        self.handlers.insert(cap.clone(), handler);
        info!("Registered capability handler: {}", cap);
    }

    /// Broadcast presence (ANNOUNCE).
    pub async fn announce(&self) -> Result<()> {
        let caps: Vec<String> = self.handlers.iter().map(|kv| kv.key().clone()).collect();
        let payload = encode_announce(
            self.identity().display_name(),
            amara_core::constants::AMARA_VERSION,
            amara_core::peer::Platform::current(),
            &caps,
            LoadClass::Idle,
            Availability::Online,
        );

        let packet = Packet::broadcast(
            PacketType::Announce,
            self.identity().peer_id(),
            amara_core::constants::DEFAULT_TTL,
            payload,
        );

        self.mesh.send_packet(packet).await
    }

    /// Advertise full capability document.
    pub async fn advertise_capabilities(&self) -> Result<()> {
        let caps: Vec<String> = self.handlers.iter().map(|kv| kv.key().clone()).collect();
        let doc = CapabilityDocument {
            name: self.identity().display_name().to_string(),
            kind: "agent".to_string(),
            capabilities: caps,
            models: vec![], // Populated dynamically in real system
            tools: vec![],
            limits: ResourceLimits::default(),
            availability: Availability::Online,
        };

        let payload = encode_capability(&doc)?;
        let packet = Packet::broadcast(
            PacketType::Capability,
            self.identity().peer_id(),
            amara_core::constants::DEFAULT_TTL,
            payload,
        );

        self.mesh.send_packet(packet).await
    }

    /// Send a task to a specific peer and wait for the result.
    pub async fn ask(&self, task: Task) -> Result<TaskResult> {
        let task_id = task.task_id;
        let target = task.target.ok_or_else(|| {
            AmaraError::Other("Task must have a target PeerId for ask()".into())
        })?;

        let (tx, rx) = tokio::sync::oneshot::channel();
        self.pending_tasks.insert(task_id, tx);

        let payload = encode_task(&task)?;
        let packet = Packet::new(
            PacketType::Task,
            self.identity().peer_id(),
            target,
            amara_core::constants::DEFAULT_TTL,
            payload,
        );

        self.mesh.send_packet(packet).await?;

        // Wait for result (in real system, add timeout)
        match rx.await {
            Ok(result) => Ok(result),
            Err(_) => Err(AmaraError::Other("Task result channel closed".into())),
        }
    }

    /// Process incoming packets from the mesh.
    pub async fn run(self: Arc<Self>) {
        let mut rx = self.mesh.subscribe();

        // Broadcast initial presence
        if let Err(e) = self.announce().await {
            warn!("Failed initial announce: {}", e);
        }

        while let Ok(packet) = rx.recv().await {
            if packet.header.source_id == self.identity().peer_id() {
                continue; // Ignore our own broadcasts
            }

            match packet.header.packet_type {
                PacketType::Task => {
                    let runtime = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = runtime.handle_incoming_task(packet).await {
                            warn!("Task processing error: {}", e);
                        }
                    });
                }
                PacketType::Result => {
                    if let Err(e) = self.handle_incoming_result(packet).await {
                        warn!("Result processing error: {}", e);
                    }
                }
                _ => {
                    // Handle other packet types (Announce, Capability, etc.)
                    // Typically update the Mesh registry here.
                }
            }
        }
    }

    async fn handle_incoming_task(self: &Arc<Self>, packet: Packet) -> Result<()> {
        let task = decode_task(&packet.payload)?;
        debug!("Received task {} from {}", task.task_id, task.issuer);

        let handler = self.handlers.get(&task.capability).ok_or_else(|| {
            AmaraError::CapabilityNotAvailable(task.capability.clone())
        })?;

        let ctx = TaskContext {
            task: task.clone(),
            runtime: self.clone(),
        };

        let start = Instant::now();
        
        // Execute task
        let mut result = match handler.handle(ctx).await {
            Ok(r) => r,
            Err(e) => {
                error!("Task execution failed: {}", e);
                TaskResult {
                    task_id: task.task_id,
                    status: TaskResultStatus::Error,
                    data: serde_json::json!({ "error": e.to_string() }),
                    agent: self.identity().peer_id(),
                    execution_time_ms: 0,
                }
            }
        };

        result.execution_time_ms = start.elapsed().as_millis() as u64;

        // Send result back
        let payload = encode_result(&result)?;
        let result_packet = Packet::new(
            PacketType::Result,
            self.identity().peer_id(),
            task.issuer, // Reply to issuer
            amara_core::constants::DEFAULT_TTL,
            payload,
        );

        self.mesh.send_packet(result_packet).await?;
        Ok(())
    }

    async fn handle_incoming_result(&self, packet: Packet) -> Result<()> {
        let result = amara_protocol::codec::decode_result(&packet.payload)?;
        debug!("Received result for task {}", result.task_id);

        if let Some((_, tx)) = self.pending_tasks.remove(&result.task_id) {
            let _ = tx.send(result);
        }
        
        Ok(())
    }
}
