export default {
  async fetch(request) {
    const url = new URL(request.url);
    const path = url.pathname;

    let version = "latest";
    try {
      const release = await fetch(
        "https://api.github.com/repos/thinhngotony/synapse/releases/latest",
        {
          headers: { "User-Agent": "synapse-worker" },
          cf: { cacheTtl: 60 },
        },
      );
      if (release.ok) {
        const data = await release.json();
        version = data.tag_name || "latest";
      }
    } catch {
      // Fallback to main if release lookup fails.
    }

    const ref = version !== "latest" ? version : "main";
    const base = `https://raw.githubusercontent.com/thinhngotony/synapse/${ref}`;

    const routes = {
      "/install": `${base}/install.sh`,
      "/install.sh": `${base}/install.sh`,
    };

    if (path === "/") {
      const displayVersion = version.replace(/^v/, "");
      return new Response(
        `Synapse ${displayVersion}

Install:
  curl -sfS https://synapse.hyberorbit.com/install | sh

Routes:
  /install
  /install.sh

Documentation: https://github.com/thinhngotony/synapse
`,
        { headers: { "Content-Type": "text/plain" } },
      );
    }

    const targetUrl = routes[path];
    if (!targetUrl) {
      return new Response("Not found", { status: 404 });
    }

    // Body must stay sh-safe: this response can be piped straight into sh.
    let response;
    try {
      response = await fetch(targetUrl, {
        cf: { cacheTtl: 0, cacheEverything: false },
      });
    } catch {
      return shError(502, `fetch of ${targetUrl} failed`);
    }
    if (!response.ok) {
      return shError(
        response.status,
        `${targetUrl} returned HTTP ${response.status}`,
      );
    }

    return new Response(response.body, {
      status: response.status,
      headers: {
        "Content-Type": "text/x-shellscript",
        "Cache-Control": "no-cache, no-store, must-revalidate",
        Pragma: "no-cache",
        Expires: "0",
        "Access-Control-Allow-Origin": "*",
      },
    });
  },
};

// ponytail: sh-safe error body so `curl | sh` aborts loudly instead of
// executing prose. Plain echo+exit, no shell metacharacters from upstream.
function shError(status, detail) {
  const safe = detail.replace(/[^\w\s.:/-]/g, "");
  return new Response(
    `#!/bin/sh
# synapse installer unavailable
echo "synapse: installer unavailable (${safe})" >&2
exit 1
`,
    {
      status,
      headers: {
        "Content-Type": "text/x-shellscript",
        "Cache-Control": "no-cache, no-store, must-revalidate",
      },
    },
  );
}
