# Robots on the mesh

AMARA treats a robot as one more agent: it announces itself, serves tools, and shares what it senses. Language
models, scripts, phones and other robots nearby can then find it, ask it to do things, watch its state and stop it,
over Wi-Fi or Bluetooth, with no broker, no cloud and no shared network config.

| Robotics pattern | AMARA | Notes |
|---|---|---|
| Topic (telemetry) | `publish` / `topics` / `watch` | Signed broadcast, a few hops, newest value cached on every node |
| Service | tool call | Sealed end to end, routed by name, `@tag` or capability |
| Action (with feedback) | streaming tool call | Progress arrives as `RESULT_CHUNK`s, then the result |
| E-stop | `estop` | Signed broadcast across the whole mesh radius |
| Discovery | announces | Robots advertise tools and tags (`robot`, `arm`, `camera`) |

## Expose a robot

Any program can be a robot's brain. With the Python SDK:

```python
from amara import Amara
mesh = Amara()

# Telemetry: a few times a second is plenty over a mesh.
mesh.publish("arm1/joints", {"j1": 0.12, "j2": 1.57})

# An action: progress streams back to the caller while it runs.
def move_to(args, call):
    for pct in plan_and_execute(args["pose"]):
        mesh.reply(call["call_id"], {"progress": pct}, partial=True)
    return {"reached": args["pose"]}

mesh.serve("move_to", "Move the arm to a named pose", move_to)
```

Or in Rust, with streaming built in:

```rust
let robot = Agent::builder("arm1")
    .tag("robot")
    .tool_fn(ToolSpec::new("move_to", "Move to a named pose"), |ctx| async move {
        for pct in [25, 50, 75, 100] {
            ctx.stream.send(json!({"progress": pct}));
        }
        Ok(json!({"reached": ctx.args["pose"]}))
    })
    .start().await?;
robot.publish("arm1/state", json!({"mode": "idle"}), None).await?;
```

Or with no code at all: a `[[tools]]` command in `config.toml` streams each line it prints.

## Use a robot

```text
You → Claude: "What is the arm doing? If it is idle, move it to the home pose."
Claude: topics(topic="arm1/*")            → arm1/state · arm1 #42 0s ago: {"mode":"idle"}
Claude: call(tool="move_to", args={"pose":"home"})   (progress streams in as notifications)
```

From a shell:

```bash
amara topics 'arm1/*' --watch          # live telemetry
amara call - move_to '{"pose":"home"}' --stream
amara stop                             # EMERGENCY STOP every robot in range
amara stop arm1 --reason "person in the cell"
```

## Emergency stop

`estop` publishes on the reserved topic `amara/estop` with the full mesh radius (8 hops). Every robot that accepts it:

1. logs it and puts a `stop` item in its inbox (every SDK and the dashboard surface it at once), and
2. runs `robot.estop_command`, if configured, with the stop as JSON on stdin.

```toml
[robot]
estop_from = "anyone"                       # anyone (default) · trusted · off
estop_command = ["ros2", "service", "call", "/estop", "std_srvs/srv/Trigger"]
```

Stopping is fail-safe, so by default anyone in range may stop a robot; set `trusted` where nuisance stops are a
concern. The stop is signed, so the robot always knows **who** stopped it.

> **The mesh e-stop is a convenience layer, not a safety-rated stop.** Radio links fail. Keep a hardware e-stop
> and a watchdog on every robot that can hurt someone.

## Practical limits

| Item | Value |
|---|---|
| Topic sample | 16 KiB encoded; default reach 2 hops (`hops` 1-8) |
| Rate | 200 packets/s per source across all traffic, so keep telemetry to ~20 Hz per topic |
| Latency | ~1 ms per LAN hop, 15-50 ms per BLE hop; run `amara bench` for the stack itself |
| Topics cached | 1024 (sender, topic) pairs per node |

## ROS 2

There is no bridge package yet. The pattern is a small node that subscribes to ROS topics and calls
`mesh.publish`, and exposes ROS services/actions as AMARA tools. Contributions welcome.
