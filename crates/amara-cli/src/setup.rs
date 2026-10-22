//! `amara setup <client>`: connect AI apps to the mesh through MCP.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Client {
    /// Claude Code (CLI)
    ClaudeCode,
    /// Claude Desktop app
    ClaudeDesktop,
    /// OpenAI Codex CLI
    Codex,
    Cursor,
    Windsurf,
    /// VS Code / GitHub Copilot (workspace .vscode/mcp.json)
    Vscode,
    /// Google Gemini CLI
    Gemini,
    /// Print a generic MCP server entry
    Generic,
}

fn exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok().or(Some(p)))
        .map(|p| {
            p.display()
                .to_string()
                .trim_start_matches(r"\\?\")
                .to_string()
        })
        .unwrap_or_else(|| "amara".into())
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn agent_name(client: Client) -> &'static str {
    match client {
        Client::ClaudeCode => "claude-code",
        Client::ClaudeDesktop => "claude",
        Client::Codex => "codex",
        Client::Cursor => "cursor",
        Client::Windsurf => "windsurf",
        Client::Vscode => "copilot",
        Client::Gemini => "gemini",
        Client::Generic => "agent",
    }
}

fn claude_desktop_config() -> PathBuf {
    if cfg!(target_os = "macos") {
        home().join("Library/Application Support/Claude/claude_desktop_config.json")
    } else if cfg!(windows) {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join("AppData/Roaming"))
            .join("Claude/claude_desktop_config.json")
    } else {
        home().join(".config/Claude/claude_desktop_config.json")
    }
}

/// (config file, top-level key) for JSON-configured clients.
fn json_target(client: Client) -> Option<(PathBuf, &'static str)> {
    match client {
        Client::ClaudeDesktop => Some((claude_desktop_config(), "mcpServers")),
        Client::Cursor => Some((home().join(".cursor/mcp.json"), "mcpServers")),
        Client::Windsurf => Some((
            home().join(".codeium/windsurf/mcp_config.json"),
            "mcpServers",
        )),
        Client::Gemini => Some((home().join(".gemini/settings.json"), "mcpServers")),
        Client::Vscode => Some((PathBuf::from(".vscode/mcp.json"), "servers")),
        _ => None,
    }
}

fn server_entry(client: Client, name: &str) -> Value {
    let args = json!(["mcp", "--name", name]);
    if client == Client::Vscode {
        json!({"type": "stdio", "command": exe(), "args": args})
    } else {
        json!({"command": exe(), "args": args})
    }
}

pub fn run(client: Client, name: Option<String>, write: bool) -> Result<()> {
    let name = name.unwrap_or_else(|| agent_name(client).to_string());
    match client {
        Client::ClaudeCode => {
            let args = [
                "mcp",
                "add",
                "amara",
                "--scope",
                "user",
                "--",
                &exe(),
                "mcp",
                "--name",
                &name,
            ];
            if write {
                let status = std::process::Command::new("claude")
                    .args(args)
                    .status()
                    .context("could not run `claude` (is Claude Code installed and on PATH?)")?;
                if !status.success() {
                    bail!("`claude mcp add` failed ({status})");
                }
                println!(
                    "✓ AMARA added to Claude Code. Restart your session, then ask: \"who's on the mesh?\""
                );
            } else {
                println!(
                    "Run this (or re-run with --write):\n\n  claude {}\n",
                    shell_join(&args)
                );
            }
        }
        Client::Codex => {
            let path = home().join(".codex/config.toml");
            let snippet = format!(
                "[mcp_servers.amara]\ncommand = {}\nargs = [\"mcp\", \"--name\", {}]\n",
                toml_str(&exe()),
                toml_str(&name)
            );
            if write {
                let existing = std::fs::read_to_string(&path).unwrap_or_default();
                if existing.contains("[mcp_servers.amara]") {
                    println!(
                        "✓ {} already has an [mcp_servers.amara] entry",
                        path.display()
                    );
                    return Ok(());
                }
                if let Some(dir) = path.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                let sep = if existing.is_empty() || existing.ends_with('\n') {
                    ""
                } else {
                    "\n"
                };
                std::fs::write(&path, format!("{existing}{sep}\n{snippet}"))?;
                println!("✓ AMARA added to {}", path.display());
            } else {
                println!(
                    "Add to {} (or re-run with --write):\n\n{snippet}",
                    path.display()
                );
            }
        }
        Client::Generic => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &json!({"mcpServers": {"amara": server_entry(client, &name)}})
                )?
            );
        }
        _ => {
            let (path, key) = json_target(client).expect("json client");
            let entry = server_entry(client, &name);
            if write {
                merge_json(&path, key, entry)?;
                println!(
                    "✓ AMARA added to {}. Restart the app to load it.",
                    path.display()
                );
            } else {
                println!(
                    "Add to {} (or re-run with --write):\n\n{}",
                    path.display(),
                    serde_json::to_string_pretty(&json!({ key: { "amara": entry } }))?
                );
            }
        }
    }
    Ok(())
}

fn merge_json(path: &Path, key: &str, entry: Value) -> Result<()> {
    let mut root: Value = match std::fs::read_to_string(path) {
        Ok(text) if !text.trim().is_empty() => serde_json::from_str(&text).with_context(|| {
            format!(
                "{} is not valid JSON; refusing to overwrite it",
                path.display()
            )
        })?,
        _ => json!({}),
    };
    if !root.is_object() {
        bail!("{} does not contain a JSON object", path.display());
    }
    if path.exists() {
        std::fs::copy(path, path.with_extension("json.bak"))?;
    }
    let servers = root
        .as_object_mut()
        .unwrap()
        .entry(key)
        .or_insert_with(|| json!({}));
    if !servers.is_object() {
        bail!("`{key}` in {} is not an object", path.display());
    }
    servers
        .as_object_mut()
        .unwrap()
        .insert("amara".into(), entry);
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(&root)? + "\n")?;
    Ok(())
}

fn toml_str(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn shell_join(args: &[&str]) -> String {
    args.iter()
        .map(|a| {
            if a.contains(' ') {
                format!("\"{a}\"")
            } else {
                a.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_preserves_existing_servers() {
        let dir = std::env::temp_dir().join(format!("amara-setup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cfg.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"x"}},"theme":"dark"}"#,
        )
        .unwrap();
        merge_json(&path, "mcpServers", json!({"command":"amara"})).unwrap();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["mcpServers"]["other"]["command"], "x");
        assert_eq!(v["mcpServers"]["amara"]["command"], "amara");
        assert_eq!(v["theme"], "dark");

        std::fs::write(&path, "{not json").unwrap();
        assert!(merge_json(&path, "mcpServers", json!({})).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn toml_escaping() {
        assert_eq!(toml_str(r"C:\bin\amara.exe"), r#""C:\\bin\\amara.exe""#);
    }
}
