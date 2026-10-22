//! A minimal agent that serves two tools on the LAN mesh.
//!
//! Run it on one machine:      cargo run -p amara-agent --example echo_agent
//! Call it from another:       amara call - reverse '{"text":"hello"}'

use amara_agent::{Agent, ToolSpec};
use serde_json::json;
use std::time::Duration;

#[tokio::main]
async fn main() -> amara_core::Result<()> {
    let agent = Agent::builder("echo-agent")
        .tag("demo")
        .tool_fn(
            ToolSpec::new("echo", "Return the arguments unchanged"),
            |ctx| async move { Ok(ctx.args) },
        )
        .tool_fn(
            ToolSpec::new("reverse", "Reverse a string").with_schema(json!({
                "type": "object", "required": ["text"], "properties": {"text": {"type": "string"}}
            })),
            |ctx| async move {
                let text = ctx.args["text"].as_str().ok_or("text is required")?;
                Ok(json!(text.chars().rev().collect::<String>()))
            },
        )
        .start()
        .await?;

    println!(
        "{} ({}) is on the mesh. Ctrl-C to stop.",
        agent.identity().display_name(),
        agent.identity().peer_id()
    );
    loop {
        tokio::time::sleep(Duration::from_secs(10)).await;
        let names: Vec<String> = agent.peers().into_iter().map(|p| p.name).collect();
        println!(
            "nearby: {}",
            if names.is_empty() {
                "nobody yet".into()
            } else {
                names.join(", ")
            }
        );
    }
}
