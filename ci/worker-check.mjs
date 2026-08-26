// Exercises the deployed Worker routes without network access.
import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";

const worker = (await import("../worker.js")).default;
const installer = await readFile(new URL("../install.sh", import.meta.url), "utf8");

globalThis.fetch = async () => {
  throw new Error("bundled Worker routes must not fetch at request time");
};

let res = await worker.fetch(new Request("https://synapse.hyberorbit.com/install"));
assert.equal(res.status, 200);
assert.equal(res.headers.get("Content-Type"), "text/x-shellscript");
assert.equal(res.headers.get("Cache-Control"), "no-cache, no-store, must-revalidate");
assert.equal(await res.text(), installer);

res = await worker.fetch(new Request("https://synapse.hyberorbit.com/install.sh"));
assert.equal(res.status, 200);
assert.equal(await res.text(), installer);

res = await worker.fetch(new Request("https://synapse.hyberorbit.com/"));
assert.equal(res.status, 200);
const help = await res.text();
assert.match(help, /curl -sfS https:\/\/synapse\.hyberorbit\.com\/install \| sh/);
assert.match(help, /Synapse latest/);
assert.match(help, /github\.com\/thinhngotony\/synapse/);
assert.match(help, /\/install/);

res = await worker.fetch(new Request("https://synapse.hyberorbit.com/nope"));
assert.equal(res.status, 404);
assert.equal(await res.text(), "Not found");

console.log("worker-check ok");
