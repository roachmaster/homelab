// Official Mermaid grammar check without a browser or remote service.
import { JSDOM } from "jsdom";
import fs from "node:fs";
import { fileURLToPath } from "node:url";
import path from "node:path";
const root = path.dirname(fileURLToPath(import.meta.url));
const window = new JSDOM("").window;
globalThis.window = window;
globalThis.document = window.document;
// Dynamic import is deliberate: DOMPurify must see the local DOM first.
const { default: mermaid } = await import("mermaid");
mermaid.initialize({ startOnLoad: false, securityLevel: "strict" });
for (const name of ["m01-contracts", "m02-bootstrap", "m03-disk-request", "m04-kernel-phases",
  "m05-foundation-modules", "m06-foundation-interfaces", "m07-job-lifecycle", "m08-remote-admission",
  "m09-template-architecture", "m10-template-render"]) {
  const source = fs.readFileSync(path.join(root, `${name}.mmd`), "utf8");
  const result = await mermaid.parse(source);
  console.log(`PASS official Mermaid syntax: ${name} (${result.diagramType})`);
}
window.close();
