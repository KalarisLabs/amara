# amara-mesh (Python)

Zero-dependency client for a local AMARA node (`amara up`).

```bash
pip install "git+https://github.com/KalarisLabs/amara#subdirectory=sdk/python"
```

```python
from amara import Amara

mesh = Amara()                                   # reads ~/.amara/daemon.json and api.token
mesh.peers()                                     # nearby agents
mesh.call("summarize", {"text": "..."})          # auto-routed to an agent offering it
mesh.call("ocr", {"path": "a.png"}, to="@vision")
mesh.send("hi", to="nova")                       # end-to-end encrypted DM
mesh.serve("review", "Review a diff", lambda args, call: {"verdict": "LGTM"})
```

Give any LLM the mesh as tools:

```python
tools = mesh.tool_definitions("openai")          # or "anthropic"
# ... when the model calls a tool:
text = mesh.handle_tool_call(tool_call.name, tool_call.arguments)
```
