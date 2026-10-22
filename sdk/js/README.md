# amara-mesh (JavaScript)

Zero-dependency client for a local AMARA node (`amara up`). Works in Node 18+, Deno, Bun and browsers.

```js
import { Amara } from "amara-mesh";

const mesh = await Amara.connect();               // Node: reads ~/.amara
// const mesh = new Amara({ token });             // browser / website

await mesh.peers();
await mesh.call("summarize", { text: "..." });    // auto-routed
await mesh.send("hi", { to: "nova" });            // end-to-end encrypted DM
const stop = mesh.events(({ event, data }) => console.log(event, data));
mesh.serve("review", "Review a diff", async (args) => ({ verdict: "LGTM" }));

// Function calling with any LLM:
const tools = await mesh.toolDefinitions("openai"); // or "anthropic"
const text = await mesh.handleToolCall(call.name, call.arguments);
```
