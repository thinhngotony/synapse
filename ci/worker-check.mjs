// Exercises worker.js routing with a stubbed global fetch. No deploy needed.
import assert from "node:assert/strict";

const worker = (await import("../worker.js")).default;

const SCRIPT = "#!/bin/sh\necho synapse installer\n";
const calls = [];

function stub(handler) {
  globalThis.fetch = async (url, init) => {
    calls.push({ url: String(url), init });
    return handler(String(url), init);
  };
}

const ok = (body, type) =>
  new Response(body, { status: 200, headers: { "Content-Type": type } });

// Happy path: release lookup succeeds, script served from the tag.
stub((url) => {
  if (url.includes("api.github.com")) {
    return ok(JSON.stringify({ tag_name: "v1.0.0" }), "application/json");
  }
  if (url.endsWith("/v1.0.0/install.sh")) return ok(SCRIPT, "text/plain");
  return new Response("nope", { status: 404 });
});

let res = await worker.fetch(new Request("https://synapse.hyberorbit.com/install"));
assert.equal(res.status, 200);
assert.equal(res.headers.get("Content-Type"), "text/x-shellscript");
assert.equal(await res.text(), SCRIPT);
assert.ok(
  calls.some((c) => c.url.includes("/v1.0.0/install.sh")),
  "resolved release tag used as ref",
);
assert.ok(
  calls.some((c) => c.init?.headers?.["User-Agent"]),
  "GitHub API called with User-Agent",
);

res = await worker.fetch(new Request("https://synapse.hyberorbit.com/"));
assert.equal(res.status, 200);
let help = await res.text();
assert.match(help, /curl -sfS https:\/\/synapse\.hyberorbit\.com\/install \| sh/);
assert.match(help, /Synapse 1\.0\.0/);
assert.match(help, /github\.com\/thinhngotony\/synapse/);
assert.match(help, /\/install/);

res = await worker.fetch(new Request("https://synapse.hyberorbit.com/nope"));
assert.equal(res.status, 404);

// Release lookup throws -> falls back to main branch.
calls.length = 0;
stub((url) => {
  if (url.includes("api.github.com")) throw new Error("api down");
  return ok(SCRIPT, "text/plain");
});
res = await worker.fetch(new Request("https://synapse.hyberorbit.com/install"));
assert.equal(res.status, 200);
assert.ok(
  calls.some((c) => c.url.endsWith("/main/install.sh")),
  "falls back to main ref",
);

// Release lookup non-2xx -> also main.
calls.length = 0;
stub((url) => {
  if (url.includes("api.github.com")) return new Response("rate limited", { status: 403 });
  return ok(SCRIPT, "text/plain");
});
res = await worker.fetch(new Request("https://synapse.hyberorbit.com/install"));
assert.equal(res.status, 200);
assert.ok(calls.some((c) => c.url.endsWith("/main/install.sh")));

// Raw script fetch fails -> sh-safe error body, non-200.
stub((url) => {
  if (url.includes("api.github.com")) {
    return ok(JSON.stringify({ tag_name: "v1.0.0" }), "application/json");
  }
  return new Response("missing", { status: 404 });
});
res = await worker.fetch(new Request("https://synapse.hyberorbit.com/install"));
assert.notEqual(res.status, 200);
let body = await res.text();
assert.match(body, /^#!\/bin\/sh/);
assert.match(body, /exit 1/);
for (const line of body.split("\n")) {
  assert.ok(
    line === "" || line.startsWith("#") || /^(echo|exit)\b/.test(line),
    `error body line not sh-safe: ${line}`,
  );
}

// Raw script fetch throws -> same guarantee.
stub((url) => {
  if (url.includes("api.github.com")) {
    return ok(JSON.stringify({ tag_name: "v1.0.0" }), "application/json");
  }
  throw new Error("network");
});
res = await worker.fetch(new Request("https://synapse.hyberorbit.com/install"));
assert.equal(res.status, 502);
assert.match(await res.text(), /exit 1/);

console.log("worker-check ok");
