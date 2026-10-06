use amara_core::peer::PeerId;
use amara_core::task::{Task, TaskResult};
use amara_crypto::identity::NodeIdentity;
use amara_mesh::{MeshConfig, MeshEngine};
use amara_runtime::{AgentRuntime, TaskContext, TaskHandler};
use amara_transport::{SimulatorHub, SimulatorNode};
use anyhow::Result;
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use std::sync::Arc;
use tokio::time::Duration;
use tracing::{info, Level};

mod tui;

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a local simulation of the AMARA mesh
    Simulate,
    /// Launch the interactive Terminal User Interface
    Tui,
}

/// A dummy task handler for testing.
struct EchoHandler;

#[async_trait]
impl TaskHandler for EchoHandler {
    fn capability(&self) -> &str {
        "echo"
    }

    async fn handle(&self, ctx: TaskContext) -> amara_core::Result<TaskResult> {
        info!("EchoHandler received task: {:?}", ctx.task.input);
        Ok(ctx.success(ctx.task.input.clone()))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(Level::INFO)
        .init();

    let cli = Cli::parse();

    match &cli.command {
        Some(Commands::Simulate) => run_simulation().await?,
        Some(Commands::Tui) => run_tui_app().await?,
        None => {
            info!("AMARA CLI - Run `amara-cli tui` for the interface.");
        }
    }

    Ok(())
}

async fn run_tui_app() -> Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel(100);
    
    // Simulate discovering agents in the background
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let mut p1 = amara_core::peer::PeerInfo::new(amara_core::peer::PeerId::from_bytes([1; 8]));
        p1.name = "Nova (Reasoning)".to_string();
        p1.capabilities = vec!["reasoning".to_string(), "coding".to_string()];
        let _ = tx.send(p1).await;

        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut p2 = amara_core::peer::PeerInfo::new(amara_core::peer::PeerId::from_bytes([2; 8]));
        p2.name = "Atlas (Vision)".to_string();
        p2.capabilities = vec!["vision".to_string(), "ocr".to_string()];
        let _ = tx.send(p2).await;
    });

    let app = tui::TuiApp::new();
    tui::run_tui(app, rx).await?;
    Ok(())
}

async fn run_simulation() -> Result<()> {
    info!("Starting AMARA mesh simulation...");

    let hub = SimulatorHub::new();

    // Node 1: Alice (Issuer)
    let id_alice = NodeIdentity::generate("alice");
    let mesh_alice = Arc::new(MeshEngine::new(id_alice.clone(), MeshConfig::default()));
    let sim_alice = SimulatorNode::new(id_alice.peer_id(), hub.clone()).await;
    mesh_alice.add_transport(Box::new(sim_alice.transport)).await;
    let rt_alice = AgentRuntime::new(mesh_alice.clone());

    // Node 2: Bob (Relay)
    let id_bob = NodeIdentity::generate("bob");
    let mesh_bob = Arc::new(MeshEngine::new(id_bob.clone(), MeshConfig::default()));
    let sim_bob = SimulatorNode::new(id_bob.peer_id(), hub.clone()).await;
    mesh_bob.add_transport(Box::new(sim_bob.transport)).await;
    let rt_bob = AgentRuntime::new(mesh_bob.clone());

    // Node 3: Charlie (Target)
    let id_charlie = NodeIdentity::generate("charlie");
    let mesh_charlie = Arc::new(MeshEngine::new(id_charlie.clone(), MeshConfig::default()));
    let sim_charlie = SimulatorNode::new(id_charlie.peer_id(), hub.clone()).await;
    mesh_charlie.add_transport(Box::new(sim_charlie.transport)).await;
    let rt_charlie = AgentRuntime::new(mesh_charlie.clone());

    // Charlie has the "echo" capability
    rt_charlie.register_handler(Box::new(EchoHandler));

    // Connect topology: Alice <-> Bob <-> Charlie
    hub.connect(id_alice.peer_id(), id_bob.peer_id(), Default::default()).await;
    hub.connect(id_bob.peer_id(), id_charlie.peer_id(), Default::default()).await;

    // Start runtimes
    mesh_alice.start().await?;
    mesh_bob.start().await?;
    mesh_charlie.start().await?;

    let r_a = rt_alice.clone();
    tokio::spawn(async move { r_a.run().await });
    
    let r_b = rt_bob.clone();
    tokio::spawn(async move { r_b.run().await });
    
    let r_c = rt_charlie.clone();
    tokio::spawn(async move { r_c.run().await });

    // Wait for discovery and routing (simulated)
    tokio::time::sleep(Duration::from_secs(1)).await;

    // Alice asks Charlie for "echo"
    info!("Alice sending task to Charlie (via Bob)...");
    
    let mut task = Task::new(
        id_alice.peer_id(),
        "echo",
        serde_json::json!({"message": "Hello across the mesh!"}),
    );
    task.target = Some(id_charlie.peer_id());

    let result = rt_alice.ask(task).await?;
    
    info!("Alice received result: {:?}", result.data);

    Ok(())
}
