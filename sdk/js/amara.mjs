// AMARA JavaScript SDK — Node 18+, Deno, Bun and browsers. Zero dependencies.
//
//   import { Amara } from "amara-mesh";
//   const mesh = await Amara.connect();            // Node: reads ~/.amara/{daemon.json,api.token}
//   const mesh = new Amara({ token });             // browser: pass the token explicitly
//   await mesh.peers();
//   await mesh.call("summarize", { text: "..." }); // auto-routed
//   mesh.serve("review", "Review a diff", async (args) => ({ verdict: "LGTM" }));

export class AmaraError extends Error {}

const DEFAULT_URL = "http://127.0.0.1:7432";

export class Amara {
  constructor({ url = DEFAULT_URL, token } = {}) {
    if (!token) throw new AmaraError("token is required (see ~/.amara/api.token)");
    this.url = url.replace(/\/$/, "");
    this.token = token;
  }

  /** Node/Deno/Bun: discover the local node from ~/.amara. */
  static async connect({ url, token } = {}) {
    const env = globalThis.process?.env ?? {};
    if (!token || !url) {
      const { readFile } = await import("node:fs/promises");
      const { homedir } = await import("node:os");
      const { join } = await import("node:path");
      const home = env.AMARA_HOME || join(homedir(), ".amara");
      token ??= env.AMARA_TOKEN ?? (await readFile(join(home, "api.token"), "utf8")).trim();
      url ??= env.AMARA_URL ?? (await readFile(join(home, "daemon.json"), "utf8").then((t) => JSON.parse(t).url, () => DEFAULT_URL));
    }
    return new Amara({ url, token });
  }

  async #req(method, path, body) {
    let res;
    try {
      res = await fetch(this.url + path, {
        method,
        headers: { Authorization: `Bearer ${this.token}`, "Content-Type": "application/json" },
        body: body === undefined ? undefined : JSON.stringify(body),
      });
    } catch (e) {
      throw new AmaraError(`cannot reach AMARA node at ${this.url}; is \`amara up\` running? (${e.message})`);
    }
    const data = await res.json().catch(() => null);
    if (!res.ok) throw new AmaraError(data?.error ?? `HTTP ${res.status}`);
    return data;
  }

  status() { return this.#req("GET", "/v1/status"); }
  peers(capability) { return this.#req("GET", "/v1/peers" + (capability ? `?capability=${encodeURIComponent(capability)}` : "")); }
  /** Encrypted DM when `to` is set, otherwise a public broadcast. */
  async send(text, { to, channel } = {}) { return (await this.#req("POST", "/v1/send", { to, text, channel })).status; }
  callRaw(tool, args = {}, { to, timeout } = {}) { return this.#req("POST", "/v1/call", { to, tool, args, timeout_secs: timeout }); }
  /** Call `tool` on `to` (name, id prefix, "@capability") or the best agent offering it. */
  async call(tool, args = {}, opts = {}) {
    const out = await this.callRaw(tool, args, opts);
    if (!out.ok) throw new AmaraError(out.data?.error ?? "remote tool failed");
    return out.data;
  }
  /**
   * Call `tool` and iterate its output as it is produced:
   *   for await (const part of mesh.callStream("generate", { prompt })) ...
   * Yields { chunk } for each partial result, then { result, ok, ms, peer }.
   */
  async *callStream(tool, args = {}, { to, timeout } = {}) {
    for await (const { event, data } of this.#sse("POST", "/v1/call", { to, tool, args, timeout_secs: timeout, stream: true })) {
      if (event === "chunk") yield { chunk: data };
      else if (event === "result") yield { result: data.data, ok: data.ok, ms: data.ms, peer: data.peer };
      else if (event === "error") throw new AmaraError(data.error ?? "call failed");
    }
  }
  /** Send a file to an agent in range: { path } on the node's disk, or { data: Uint8Array|ArrayBuffer, name }. */
  async sendFile(to, { path, data, name } = {}) {
    let body;
    if (path) body = { to, path };
    else if (data) {
      const bytes = data instanceof Uint8Array ? data : new Uint8Array(data);
      let bin = "";
      for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
      body = { to, name: name ?? "file", data_base64: btoa(bin) };
    } else throw new AmaraError("give { path } or { data, name }");
    return (await this.#req("POST", "/v1/files", body)).receipt;
  }
  /** Publish a value on a topic (e.g. "rover1/battery") to agents within `hops` (default 2). */
  async publish(topic, data = null, { hops } = {}) { return (await this.#req("POST", "/v1/publish", { topic, data, hops })).seq; }
  /** Newest value per (sender, topic). pattern: exact, "rover1/*" or "*". */
  topics(pattern) { return this.#req("GET", "/v1/topics" + (pattern ? `?topic=${encodeURIComponent(pattern)}` : "")); }
  /** Iterate samples matching `pattern` as they arrive: for await (const s of mesh.watch("rover1/*")) ... */
  async *watch(pattern = "*") {
    for await (const { event, data } of this.#sse("GET", `/v1/topics/watch?topic=${encodeURIComponent(pattern)}`)) {
      if (event === "sample") yield data;
    }
  }
  /** EMERGENCY STOP every robot in range, or only `to`. */
  async estop(reason = "emergency stop", { to } = {}) { return (await this.#req("POST", "/v1/estop", { to, reason })).status; }
  /** Files other agents sent to this node, newest first. */
  files() { return this.#req("GET", "/v1/files"); }
  /** Contents of a received file as an ArrayBuffer. */
  async download(name) {
    const res = await fetch(`${this.url}/v1/files/${encodeURIComponent(name)}`, { headers: { Authorization: `Bearer ${this.token}` } });
    if (!res.ok) throw new AmaraError(`download failed: HTTP ${res.status}`);
    return res.arrayBuffer();
  }

  async *#sse(method, path, body) {
    const res = await fetch(this.url + path, {
      method,
      headers: { Authorization: `Bearer ${this.token}`, "Content-Type": "application/json" },
      body: body === undefined ? undefined : JSON.stringify(body),
    });
    if (!res.ok) {
      const data = await res.json().catch(() => null);
      throw new AmaraError(data?.error ?? `HTTP ${res.status}`);
    }
    const reader = res.body.getReader();
    const dec = new TextDecoder();
    let buf = "";
    for (;;) {
      const { value, done } = await reader.read();
      if (done) return;
      buf += dec.decode(value, { stream: true }).replace(/\r\n/g, "\n");
      let idx;
      while ((idx = buf.indexOf("\n\n")) >= 0) {
        const block = buf.slice(0, idx);
        buf = buf.slice(idx + 2);
        let event = "message", data = "";
        for (const line of block.split("\n")) {
          if (line.startsWith("event:")) event = line.slice(6).trim();
          else if (line.startsWith("data:")) data += line.slice(5).trim();
        }
        if (data) yield { event, data: JSON.parse(data) };
      }
    }
  }

  card(peer) { return this.#req("GET", `/v1/card/${encodeURIComponent(peer)}`); }
  inbox({ wait = 0, max = 50 } = {}) { return this.#req("GET", `/v1/inbox?wait=${wait}&max=${max}`); }
  /** Answer a call. `partial: true` streams `result` as progress and keeps the call open. */
  reply(callId, { result = null, error, partial = false } = {}) {
    const body = error !== undefined ? { call_id: callId, error } : { call_id: callId, result };
    if (partial) body.partial = true;
    return this.#req("POST", "/v1/reply", body);
  }
  expose(name, description, inputSchema) { return this.#req("POST", "/v1/tools", { name, description, input_schema: inputSchema }); }
  unexpose(name) { return this.#req("DELETE", `/v1/tools/${encodeURIComponent(name)}`); }
  toolDefinitions(format = "openai") { return this.#req("GET", `/v1/schema?format=${format}`); }

  /** Live events; returns a function that stops the stream. onEvent({event, data}). */
  events(onEvent) {
    const ctrl = new AbortController();
    (async () => {
      const res = await fetch(`${this.url}/v1/events`, { headers: { Authorization: `Bearer ${this.token}` }, signal: ctrl.signal });
      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buf = "";
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buf += dec.decode(value, { stream: true });
        let idx;
        while ((idx = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          let event = "message", data = "";
          for (const line of block.split("\n")) {
            if (line.startsWith("event:")) event = line.slice(6).trim();
            else if (line.startsWith("data:")) data += line.slice(5).trim();
          }
          if (data) onEvent({ event, data: JSON.parse(data) });
        }
      }
    })().catch((e) => { if (e.name !== "AbortError") console.error("amara events:", e); });
    return () => ctrl.abort();
  }

  /** Offer a tool and answer calls until `signal` aborts. handler(args, call) => JSON. */
  async serve(name, description, handler, { inputSchema, signal, onMessage } = {}) {
    await this.expose(name, description, inputSchema);
    while (!signal?.aborted) {
      for (const item of await this.inbox({ wait: 30 })) {
        if (item.kind === "call" && item.tool === name) {
          try { await this.reply(item.call_id, { result: await handler(item.args, item) }); }
          catch (e) { await this.reply(item.call_id, { error: String(e?.message ?? e) }); }
        } else if (item.kind === "message") onMessage?.(item);
      }
    }
  }

  /** Execute a model's `amara_*` tool call; returns compact text for the model. */
  async handleToolCall(name, args) {
    if (typeof args === "string") args = JSON.parse(args || "{}");
    args ??= {};
    try {
      let r;
      switch (name.replace(/^amara_/, "")) {
        case "peers": r = await this.peers(args.capability); break;
        case "send": r = await this.send(args.text, args); break;
        case "inbox": r = await this.inbox({ wait: args.wait ?? 0, max: args.max ?? 20 }); break;
        case "call": r = await this.callRaw(args.tool, args.args ?? {}, { to: args.to, timeout: args.timeout }); break;
        case "card": r = await this.card(args.to); break;
        case "expose": r = args.remove ? (await this.unexpose(args.name), "removed") : (await this.expose(args.name, args.description ?? "", args.input_schema), "exposed"); break;
        case "reply": await this.reply(args.call_id, { result: args.result, error: args.error, partial: !!args.partial }); r = "reply sent"; break;
        case "send_file": r = await this.sendFile(args.to, { path: args.path }); break;
        case "topics": r = await this.topics(args.topic); break;
        case "publish": r = `published #${await this.publish(args.topic, args.data ?? null, { hops: args.hops })}`; break;
        case "estop": r = await this.estop(args.reason ?? "emergency stop", { to: args.to }); break;
        default: return `error: unknown tool ${name}`;
      }
      return typeof r === "string" ? r : JSON.stringify(r);
    } catch (e) {
      return `error: ${e.message}`;
    }
  }
}

export default Amara;
