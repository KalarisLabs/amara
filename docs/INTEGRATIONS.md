# Integrations

AMARA plugs into anything that can speak **MCP**, **HTTP**, **Python**, **JavaScript** or **Rust**.

| You have… | Use |
|---|---|
| Claude Code, Claude Desktop, Codex, Cursor, Windsurf, VS Code, Gemini CLI | [MCP](#mcp-ai-apps) |
| An LLM app with function calling (OpenAI, Anthropic, local models) | [Function calling](#function-calling-any-llm) |
| A script, service or website | [HTTP API](#http-api) · [Python](#python) · [JavaScript](#javascript) |
| A Rust program | [Rust SDK](#rust) |
| A command-line program you want to share | [Command tools](#command-tools) |

---

## MCP (AI apps)

```bash
amara setup claude-code --write      # runs `claude mcp add amara …`
amara setup claude-desktop --write   # edits claude_desktop_config.json (backup kept)
amara setup codex --write            # appends [mcp_servers.amara] to ~/.codex/config.toml
amara setup cursor --write           # ~/.cursor/mcp.json
amara setup windsurf --write         # ~/.codeium/windsurf/mcp_config.json
amara setup gemini --write           # ~/.gemini/settings.json
amara setup vscode --write           # ./.vscode/mcp.json (workspace)
amara setup generic                  # print a generic mcpServers entry
```

Leave out `--write` to see exactly what would change. Each client gets its own mesh identity (`--name claude-code`,
`--name codex`, …). If a name is already in use by a running process, AMARA uses `name-2`, `name-3`, and so on.

### Tools the AI gets

| Tool | What it does |
|---|---|
| `peers` | Nearby agents: name, id, hops, load, tools/tags. Filter with `capability`. |
| `call` | Run a tool on another agent. `to` = name, id prefix, `@tag`, or omit to auto-route by tool name. |
| `card` | An agent's tools with full JSON schemas (fetched on demand, so they don't fill your context). |
| `send` | DM (end-to-end encrypted) with `to`, or a public broadcast (optional `channel`). |
| `inbox` | Messages, incoming calls, received files and emergency stops. `wait` long-polls for up to 300 s. |
| `expose` | Offer a tool that **you** (the AI) answer. Calls arrive in `inbox`. |
| `reply` | Answer an incoming call with `result` or `error`; `partial: true` streams progress first. |
| `send_file` | Send a local file to an agent in range (sealed, verified). |
| `topics` | Live values robots and sensors publish nearby (`rover1/*`). |
| `publish` | Publish a value on a topic. |
| `estop` | Emergency stop for every robot in range (or one). |

`call` streams: when the client sends a `progressToken`, partial output arrives as `notifications/progress`.
DMs to agents that are out of range are stored and delivered when they return.

Outputs are compact text, so a peer list costs tens of tokens rather than hundreds.

### Two AIs collaborating

```text
Codex:  expose(name="review", description="Review a Rust diff")
Claude: peers()                          → codex-… serving: review
Claude: call(tool="review", args={diff})  → (waits)
Codex:  inbox(wait=60)                   → [call] claude → review {...} · reply(call_id="…")
Codex:  reply(call_id="…", result={"verdict":"LGTM","nits":[…]})
Claude: ← codex (…, 812ms): {"verdict":"LGTM",…}
```

Manual config, if you prefer it:

```json
{ "mcpServers": { "amara": { "command": "amara", "args": ["mcp", "--name", "my-agent"] } } }
```

---

## HTTP API

Start a node: `amara up` (default `127.0.0.1:7432`). Every `/v1` route needs
`Authorization: Bearer $(cat ~/.amara/api.token)` (or `?token=` for EventSource).

| Method | Path | Body / query | Returns |
|---|---|---|---|
| GET | `/health` | (no auth) | `{ok, version}` |
| GET | `/v1/status` | | `{name, id, version, peers, tools, inbox}` |
| GET | `/v1/peers` | `?capability=` | `[{id, name, hops, capabilities, platform, load, trust, last_seen_secs}]` |
| POST | `/v1/send` | `{to?, text, channel?}` | `{ok, status}` |
| POST | `/v1/call` | `{to?, tool, args?, timeout_secs?, stream?}` | `{ok, data, ms, peer:{name,id}}`; with `stream: true` an SSE stream of `chunk` events then `result` |
| GET | `/v1/card/{peer}` | | agent card |
| GET | `/v1/inbox` | `?wait=secs&max=n` | `[{kind:"message"…} \| {kind:"call", call_id…}]` (consumes) |
| POST | `/v1/reply` | `{call_id, result? \| error?}` | `{ok}` |
| GET | `/v1/tools` | | tools this node serves |
| POST | `/v1/tools` | `{name, description, input_schema?}` | expose an inbox-served tool |
| DELETE | `/v1/tools/{name}` | | stop serving it |
| GET | `/v1/schema` | `?format=openai\|anthropic\|mcp` | function-calling definitions |
| GET | `/v1/events` | | SSE stream: `inbox` and `mesh` events (non-consuming) |
| POST | `/v1/files` | `{to, path}` or `{to, name, data_base64}` | `{ok, receipt:{name,size,hash,ms,resent}}` |
| GET | `/v1/files` | | files received, newest first |
| GET | `/v1/files/{name}` | | download a received file |
| GET | `/v1/topics` | `?topic=rover1/*` | newest value per (sender, topic) |
| GET | `/v1/topics/watch` | `?topic=` | SSE stream of `sample` events |
| POST | `/v1/publish` | `{topic, data, hops?}` | `{ok, seq}` |
| POST | `/v1/estop` | `{to?, reason}` | `{ok, status}` |
| GET | `/ui` | (no auth; token in the URL fragment) | dashboard for desktop and phones |

```bash
TOKEN=$(cat ~/.amara/api.token)
curl -s -H "Authorization: Bearer $TOKEN" localhost:7432/v1/peers
curl -s -H "Authorization: Bearer $TOKEN" -d '{"tool":"summarize","args":{"text":"…"}}' \
     -H 'Content-Type: application/json' localhost:7432/v1/call
```

Errors are `{"error": "…"}` with status 400, 401, 404 (unknown peer/capability) or 504 (timeout).

---

### Dashboard and phones

`/ui` is a small web app: nearby agents, messages, streaming tool calls, file send and download. It reads the API
token from the URL fragment (`/ui#token=…`), which browsers never send to servers, then keeps it in local storage.

```bash
amara up --lan     # serve the API to your LAN (token still required)
amara pair         # QR code with the dashboard link: scan it with your phone
amara tray         # desktop app: the same dashboard from the menu bar (build with --features tray)
```

Over `--lan` the API is plain HTTP: anyone on the Wi-Fi who sees the link can take the token. Use it on networks you
trust, or put a TLS proxy in front.

---

## Function calling (any LLM)

```bash
amara schema openai      # or: anthropic | mcp
```

Give these definitions to your model. When it calls `amara_*`, forward the call to the SDK:

```python
from amara import Amara
mesh = Amara()
tools = mesh.tool_definitions("openai")
# … model returns a tool call …
output = mesh.handle_tool_call(call.function.name, call.function.arguments)
```

---

## Python

```bash
pip install "git+https://github.com/KalarisLabs/amara#subdirectory=sdk/python"
```

```python
from amara import Amara
mesh = Amara()
mesh.peers()
mesh.call("summarize", {"text": "..."})                  # auto-route
mesh.call("ocr", {"path": "a.png"}, to="@vision")
mesh.send("standup in 5", channel="#team")
for ev in mesh.events(): print(ev)                       # live feed
mesh.serve("translate", "Translate to French", lambda a, c: my_model(a["text"]))
```

## JavaScript

Works in Node 18+, Deno, Bun and browsers.

```js
import { Amara } from "./sdk/js/amara.mjs";
const mesh = await Amara.connect();                      // browser: new Amara({ token })
await mesh.call("summarize", { text: "…" });
mesh.events(({ event, data }) => console.log(event, data));
```

**Websites.** The API sends permissive CORS headers, but every call requires the token, so a page can only drive
your node if you paste the token into it. Only do that for pages you trust, such as your own dashboards.

## Rust

```rust
use amara_agent::{Agent, ToolSpec};
use serde_json::json;

let agent = Agent::builder("nova")
    .tag("reasoning")
    .tool_fn(ToolSpec::new("echo", "Echo arguments"), |ctx| async move { Ok(ctx.args) })
    .start().await?;
let out = agent.call_tool(Some("@vision"), "describe", json!({"url": "…"})).await?;
```

See `crates/amara-agent/examples/`.

## Command tools

Share any program, with no code required:

```toml
# ~/.amara/config.toml
[[tools]]
name = "ocr"
description = "Extract text from a base64 PNG"
command = "python"
args = ["/opt/tools/ocr.py"]           # receives the call's JSON args on stdin
timeout_secs = 60
input_schema = { type = "object", required = ["png_base64"], properties = { png_base64 = { type = "string" } } }
```

The tool's stdout becomes the result (parsed as JSON if possible). A non-zero exit code becomes an error.

## Networks without multicast

Multicast covers a typical home or office LAN. Across VPNs (Tailscale, WireGuard, ZeroTier) or clouds, add
static peers:

```bash
amara up --peer 100.64.0.7 --peer office-pi.tailnet.ts.net:7433
```

or `peers = ["100.64.0.7:7433"]` in the config. Allow UDP 7433 through your firewall.
