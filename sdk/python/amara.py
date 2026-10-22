"""AMARA Python SDK: talk to a local `amara up` node. Zero dependencies.

    from amara import Amara
    mesh = Amara()                      # finds ~/.amara/daemon.json + api.token
    print(mesh.peers())
    print(mesh.call("summarize", {"text": "..."}))          # auto-routed
    mesh.send("hello everyone")                             # public broadcast
    mesh.serve("review", "Review a diff", lambda args, call: {"verdict": "LGTM"})
    for part in mesh.call_stream("generate", {"prompt": "..."}):   # streaming output
        print(part)
    mesh.send_file("nova", path="report.pdf")                # verified file transfer

Function calling with any LLM (OpenAI, Anthropic, ...):

    tools = mesh.tool_definitions("openai")     # or "anthropic"
    result = mesh.handle_tool_call(name, args)  # dispatch the model's tool call
"""

from __future__ import annotations

import json
import os
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Callable, Dict, Iterator, List, Optional

__all__ = ["Amara", "AmaraError"]
__version__ = "0.3.0"


class AmaraError(RuntimeError):
    """Raised when the node returns an error or a remote tool fails."""


def _home() -> Path:
    return Path(os.environ.get("AMARA_HOME") or Path.home() / ".amara")


class Amara:
    def __init__(self, url: Optional[str] = None, token: Optional[str] = None, timeout: float = 3700):
        home = _home()
        if url is None:
            url = os.environ.get("AMARA_URL")
        if url is None:
            try:
                url = json.loads((home / "daemon.json").read_text())["url"]
            except (OSError, ValueError, KeyError):
                url = "http://127.0.0.1:7432"
        if token is None:
            token = os.environ.get("AMARA_TOKEN")
        if token is None:
            try:
                token = (home / "api.token").read_text().strip()
            except OSError as e:
                raise AmaraError("no API token: start a node with `amara up` or pass token=") from e
        self.url = url.rstrip("/")
        self.token = token
        self.timeout = timeout

    # ── transport ────────────────────────────────────────────────────────────
    def _request(self, method: str, path: str, body: Any = None) -> Any:
        data = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(
            self.url + path,
            data=data,
            method=method,
            headers={"Authorization": f"Bearer {self.token}", "Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                return json.loads(resp.read() or b"null")
        except urllib.error.HTTPError as e:
            try:
                msg = json.loads(e.read()).get("error", str(e))
            except ValueError:
                msg = str(e)
            raise AmaraError(msg) from None
        except urllib.error.URLError as e:
            raise AmaraError(f"cannot reach AMARA node at {self.url} ({e.reason}); is `amara up` running?") from None

    # ── API ──────────────────────────────────────────────────────────────────
    def status(self) -> Dict[str, Any]:
        return self._request("GET", "/v1/status")

    def peers(self, capability: Optional[str] = None) -> List[Dict[str, Any]]:
        q = f"?capability={urllib.parse.quote(capability)}" if capability else ""
        return self._request("GET", "/v1/peers" + q)

    def send(self, text: str, to: Optional[str] = None, channel: Optional[str] = None) -> str:
        """Encrypted DM when `to` is given, otherwise a public broadcast."""
        return self._request("POST", "/v1/send", {"to": to, "text": text, "channel": channel})["status"]

    def call_raw(self, tool: str, args: Optional[dict] = None, to: Optional[str] = None, timeout: Optional[int] = None) -> Dict[str, Any]:
        return self._request("POST", "/v1/call", {"to": to, "tool": tool, "args": args or {}, "timeout_secs": timeout})

    def call(self, tool: str, args: Optional[dict] = None, to: Optional[str] = None, timeout: Optional[int] = None) -> Any:
        """Call `tool` on `to` (name, id prefix, "@capability") or the best agent offering it."""
        out = self.call_raw(tool, args, to, timeout)
        if not out.get("ok"):
            raise AmaraError((out.get("data") or {}).get("error", "remote tool failed"))
        return out.get("data")

    def call_stream(self, tool: str, args: Optional[dict] = None, to: Optional[str] = None,
                    timeout: Optional[int] = None) -> Iterator[Dict[str, Any]]:
        """Call `tool` and yield its output as it is produced.

        Yields {"chunk": data} for each partial result, then one final
        {"result": data, "ok": bool, "ms": int, "peer": {...}}.
        """
        body = {"to": to, "tool": tool, "args": args or {}, "timeout_secs": timeout, "stream": True}
        for event in self._sse("POST", "/v1/call", body):
            if event["event"] == "chunk":
                yield {"chunk": event["data"]}
            elif event["event"] == "result":
                out = event["data"]
                yield {"result": out.get("data"), "ok": out.get("ok"), "ms": out.get("ms"), "peer": out.get("peer")}
            elif event["event"] == "error":
                raise AmaraError(event["data"].get("error", "call failed"))

    def send_file(self, to: str, path: Optional[str] = None, data: Optional[bytes] = None,
                  name: Optional[str] = None) -> Dict[str, Any]:
        """Send a file to an agent in range, from a local `path` or from `data` bytes."""
        if path is not None:
            body: Dict[str, Any] = {"to": to, "path": str(Path(path).resolve())}
        elif data is not None:
            import base64
            body = {"to": to, "name": name or "file", "data_base64": base64.b64encode(data).decode()}
        else:
            raise AmaraError("give path= or data=")
        return self._request("POST", "/v1/files", body)["receipt"]

    def files(self) -> List[Dict[str, Any]]:
        """Files other agents have sent to this node, newest first."""
        return self._request("GET", "/v1/files")

    def download(self, name: str) -> bytes:
        req = urllib.request.Request(self.url + "/v1/files/" + urllib.parse.quote(name, safe=""),
                                     headers={"Authorization": f"Bearer {self.token}"})
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as resp:
                return resp.read()
        except urllib.error.HTTPError as e:
            raise AmaraError(f"download failed: HTTP {e.code}") from None

    # ── topics and robots ────────────────────────────────────────────────────
    def publish(self, topic: str, data: Any = None, hops: Optional[int] = None) -> int:
        """Publish a value on a topic (e.g. "rover1/battery") to agents within `hops` (default 2)."""
        return self._request("POST", "/v1/publish", {"topic": topic, "data": data, "hops": hops})["seq"]

    def topics(self, pattern: Optional[str] = None) -> List[Dict[str, Any]]:
        """Newest value per (sender, topic). `pattern`: exact, "rover1/*" or "*"."""
        q = f"?topic={urllib.parse.quote(pattern)}" if pattern else ""
        return self._request("GET", "/v1/topics" + q)

    def watch(self, pattern: str = "*") -> Iterator[Dict[str, Any]]:
        """Yield samples matching `pattern` as they arrive."""
        for event in self._sse("GET", f"/v1/topics/watch?topic={urllib.parse.quote(pattern)}"):
            if event["event"] == "sample":
                yield event["data"]

    def estop(self, reason: str = "emergency stop", to: Optional[str] = None) -> str:
        """EMERGENCY STOP every robot in range, or only `to`."""
        return self._request("POST", "/v1/estop", {"to": to, "reason": reason})["status"]

    def card(self, peer: str) -> Dict[str, Any]:
        return self._request("GET", "/v1/card/" + urllib.parse.quote(peer, safe=""))

    def inbox(self, wait: int = 0, max: int = 50) -> List[Dict[str, Any]]:
        return self._request("GET", f"/v1/inbox?wait={int(wait)}&max={int(max)}")

    def reply(self, call_id: str, result: Any = None, error: Optional[str] = None, partial: bool = False) -> None:
        """Answer a call. `partial=True` streams `result` as progress and keeps the call open
        (only for calls whose inbox item has "stream": true)."""
        body: Dict[str, Any] = {"call_id": call_id}
        if error is not None:
            body["error"] = error
        else:
            body["result"] = result
        if partial:
            body["partial"] = True
        self._request("POST", "/v1/reply", body)

    def expose(self, name: str, description: str, input_schema: Optional[dict] = None) -> None:
        self._request("POST", "/v1/tools", {"name": name, "description": description, "input_schema": input_schema})

    def unexpose(self, name: str) -> None:
        self._request("DELETE", "/v1/tools/" + urllib.parse.quote(name, safe=""))

    def events(self) -> Iterator[Dict[str, Any]]:
        """Yield live events: {"event": "inbox"|"mesh", "data": {...}} (does not consume the inbox)."""
        return self._sse("GET", "/v1/events")

    def _sse(self, method: str, path: str, body: Any = None) -> Iterator[Dict[str, Any]]:
        data_bytes = None if body is None else json.dumps(body).encode()
        req = urllib.request.Request(self.url + path, data=data_bytes, method=method, headers={
            "Authorization": f"Bearer {self.token}", "Content-Type": "application/json"})
        try:
            resp = urllib.request.urlopen(req, timeout=None)
        except urllib.error.HTTPError as e:
            try:
                msg = json.loads(e.read()).get("error", str(e))
            except ValueError:
                msg = str(e)
            raise AmaraError(msg) from None
        with resp:
            event, data = "message", []
            for raw in resp:
                line = raw.decode().rstrip("\r\n")
                if not line:
                    if data:
                        yield {"event": event, "data": json.loads("\n".join(data))}
                    event, data = "message", []
                elif line.startswith("event:"):
                    event = line[6:].strip()
                elif line.startswith("data:"):
                    data.append(line[5:].strip())

    def serve(self, name: str, description: str, handler: Callable[[dict, dict], Any],
              input_schema: Optional[dict] = None, on_message: Optional[Callable[[dict], None]] = None) -> None:
        """Offer a tool and answer calls forever. `handler(args, call)` returns JSON or raises."""
        self.expose(name, description, input_schema)
        while True:
            for item in self.inbox(wait=60):
                if item["kind"] == "call" and item["tool"] == name:
                    try:
                        self.reply(item["call_id"], result=handler(item["args"], item))
                    except Exception as e:  # report failures to the caller
                        self.reply(item["call_id"], error=str(e))
                elif item["kind"] == "message" and on_message:
                    on_message(item)

    # ── LLM function calling ─────────────────────────────────────────────────
    def tool_definitions(self, fmt: str = "openai") -> List[Dict[str, Any]]:
        """AMARA tools as function definitions ("openai" or "anthropic")."""
        return self._request("GET", f"/v1/schema?format={urllib.parse.quote(fmt)}")

    def handle_tool_call(self, name: str, args: Any) -> str:
        """Execute a model's `amara_*` tool call; returns compact text for the model."""
        if isinstance(args, str):
            args = json.loads(args or "{}")
        args = args or {}
        name = name.removeprefix("amara_")
        try:
            if name == "peers":
                result: Any = self.peers(args.get("capability"))
            elif name == "send":
                result = self.send(args["text"], args.get("to"), args.get("channel"))
            elif name == "inbox":
                result = self.inbox(args.get("wait", 0), args.get("max", 20))
            elif name == "call":
                result = self.call_raw(args["tool"], args.get("args"), args.get("to"), args.get("timeout"))
            elif name == "card":
                result = self.card(args["to"])
            elif name == "expose":
                if args.get("remove"):
                    self.unexpose(args["name"])
                    result = "removed"
                else:
                    self.expose(args["name"], args.get("description", ""), args.get("input_schema"))
                    result = "exposed"
            elif name == "reply":
                self.reply(args["call_id"], args.get("result"), args.get("error"), bool(args.get("partial")))
                result = "reply sent"
            elif name == "send_file":
                result = self.send_file(args["to"], path=args["path"])
            elif name == "topics":
                result = self.topics(args.get("topic"))
            elif name == "publish":
                result = f"published #{self.publish(args['topic'], args.get('data'), args.get('hops'))}"
            elif name == "estop":
                result = self.estop(args.get("reason", "emergency stop"), args.get("to"))
            else:
                return f"error: unknown tool {name}"
        except (AmaraError, KeyError) as e:
            return f"error: {e}"
        return result if isinstance(result, str) else json.dumps(result, separators=(",", ":"))


if __name__ == "__main__":  # quick smoke test: python amara.py
    mesh = Amara()
    print(json.dumps(mesh.status(), indent=2))
    for p in mesh.peers():
        print(f"{p['name']:<16} {p['id'][:8]} {p['hops']} hop(s)  {', '.join(p['capabilities'])}")
