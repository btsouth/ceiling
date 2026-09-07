const REPO = "btsouth/ceiling";
const CACHE_SECONDS = 300;
const SESSION_MAX_AGE = 60 * 60 * 24 * 7;
const SESSION_MESSAGE = "ceiling-admin-session-v1";
const VISITOR_ID_ROTATION_MS = 30 * 24 * 60 * 60 * 1000;
const ALLOWED_EVENTS = new Set(["$pageview", "download_clicked", "github_clicked"]);
const LATEST_RELEASE_PAGE = `https://github.com/${REPO}/releases/latest`;
const PUBLIC_HOST = "ceiling.win";
const DOWNLOAD_ROUTES = {
  "/download": /-Setup\.exe$/i,
  "/download/portable": /-portable\.exe$/i,
};

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);

    if (url.hostname === `www.${PUBLIC_HOST}`) {
      url.hostname = PUBLIC_HOST;
      url.protocol = "https:";
      return Response.redirect(url.toString(), 301);
    }

    if (request.method === "POST" && url.pathname === "/api/events") {
      return captureEvent(request, env, ctx);
    }

    if (url.pathname === "/api/stars") {
      if (request.method !== "GET") return methodNotAllowed();
      return starCountResponse(env, ctx);
    }

    if (url.pathname === "/admin/session" && request.method === "POST") {
      return createSession(request, env);
    }

    if (url.pathname === "/admin/logout" && request.method === "POST") {
      return new Response(null, {
        status: 204,
        headers: { "Set-Cookie": expiredSessionCookie() },
      });
    }

    if (url.pathname === "/admin/api/metrics") {
      if (!(await hasAdminSession(request, env))) return unauthorizedJson();
      if (request.method !== "GET") return methodNotAllowed();
      return metricsResponse(env, ctx, url.searchParams.has("refresh"));
    }

    if (url.pathname === "/admin" || url.pathname === "/admin/" || url.pathname === "/admin.html") {
      if (!(await hasAdminSession(request, env))) return adminLoginPage(Boolean(env.ADMIN_TOKEN));
      const assetUrl = new URL(request.url);
      assetUrl.pathname = "/_private/admin-dashboard.txt";
      return secureAdminResponse(await env.ASSETS.fetch(new Request(assetUrl, request)));
    }

    if (url.pathname.startsWith("/_private/")) return new Response("Not found", { status: 404 });

    const downloadPattern = DOWNLOAD_ROUTES[url.pathname.replace(/\/+$/, "") || "/"];
    if (downloadPattern) {
      const target = await latestAssetUrl(downloadPattern, env, ctx).catch(() => LATEST_RELEASE_PAGE);
      return Response.redirect(target, 302);
    }

    return publicAssetResponse(await env.ASSETS.fetch(request));
  },
};

function publicAssetResponse(response) {
  const headers = new Headers(response.headers);
  headers.set("Referrer-Policy", "strict-origin-when-cross-origin");
  headers.set("X-Content-Type-Options", "nosniff");
  headers.set("Permissions-Policy", "camera=(), microphone=(), geolocation=()");
  return new Response(response.body, {
    status: response.status,
    statusText: response.statusText,
    headers,
  });
}

// Resolve the newest signed installer at click time so the download button never
// needs editing when a release ships. Cached at the edge; falls back to the
// releases page on any error via the caller's catch.
async function latestAssetUrl(pattern, env, ctx) {
  const cache = caches.default;
  const cacheKey = new Request("https://ceiling.internal/latest-release-download-v1");
  let cached = await cache.match(cacheKey);
  let data;
  if (cached) {
    data = await cached.json();
  } else {
    const headers = { "User-Agent": "ceiling-site", Accept: "application/vnd.github+json" };
    if (env.GITHUB_TOKEN) headers.Authorization = `Bearer ${env.GITHUB_TOKEN}`;
    const res = await fetch(`https://api.github.com/repos/${REPO}/releases/latest`, {
      headers,
      signal: AbortSignal.timeout(3000),
    });
    if (!res.ok) throw new Error(`github api ${res.status}`);
    const body = await res.text();
    const store = new Response(body, {
      headers: { "Content-Type": "application/json", "Cache-Control": `public, max-age=${CACHE_SECONDS}` },
    });
    ctx.waitUntil(cache.put(cacheKey, store.clone()));
    data = JSON.parse(body);
  }
  const asset = (data.assets || []).find((a) => pattern.test(a.name));
  if (!asset) throw new Error("no matching asset");
  return asset.browser_download_url;
}

// Public star count for the "Star on GitHub" button. Edge-cached so the button
// stays fast and never hammers the GitHub API; a fetch failure returns a null
// count (uncached) so the client simply hides the badge and shows the plain button.
async function starCountResponse(env, ctx) {
  const cache = caches.default;
  const cacheKey = new Request("https://ceiling.internal/star-count-v1");
  const cached = await cache.match(cacheKey);
  if (cached) return cached;

  let stars = null;
  try {
    const headers = { "User-Agent": "ceiling-site", Accept: "application/vnd.github+json" };
    if (env.GITHUB_TOKEN) headers.Authorization = `Bearer ${env.GITHUB_TOKEN}`;
    const res = await fetch(`https://api.github.com/repos/${REPO}`, {
      headers,
      signal: AbortSignal.timeout(3000),
    });
    if (res.ok) {
      const data = await res.json();
      const parsed = numberValue(data.stargazers_count);
      if (Number.isFinite(parsed)) stars = parsed;
    }
  } catch {
    stars = null;
  }

  const response = new Response(JSON.stringify({ stars }), {
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": stars === null ? "no-store" : `public, max-age=${CACHE_SECONDS}`,
    },
  });
  if (stars !== null) ctx.waitUntil(cache.put(cacheKey, response.clone()));
  return response;
}

async function captureEvent(request, env, ctx) {
  if (!sameOrigin(request) || !isJson(request)) return new Response(null, { status: 204 });

  let payload;
  try {
    payload = await request.json();
  } catch {
    return new Response(null, { status: 204 });
  }

  const event = typeof payload.event === "string" ? payload.event : "";
  if (!ALLOWED_EVENTS.has(event) || !env.POSTHOG_PUBLIC_KEY || !env.ANALYTICS_SALT) {
    return new Response(null, { status: 204 });
  }

  const requestUrl = new URL(request.url);
  const pathname = safePath(payload.pathname);
  const referrer = safeReferrer(payload.referrer, requestUrl.origin);
  const distinctId = await anonymousVisitorId(request, env.ANALYTICS_SALT);
  const captureHost = (env.POSTHOG_CAPTURE_HOST || "https://us.i.posthog.com").replace(/\/$/, "");

  const properties = {
    distinct_id: distinctId,
    $host: requestUrl.hostname,
    $pathname: pathname,
    $current_url: `${requestUrl.origin}${pathname}`,
    $referrer: referrer.url,
    $referring_domain: referrer.domain,
    source: "ceiling.win",
  };

  if (event === "download_clicked") {
    properties.asset = safeLabel(payload.asset, 100);
    properties.release = safeLabel(payload.release, 40);
  }

  const capture = fetch(`${captureHost}/i/v0/e/`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ api_key: env.POSTHOG_PUBLIC_KEY, event, properties }),
  }).catch(() => undefined);
  ctx.waitUntil(capture);
  return new Response(null, { status: 204 });
}

async function createSession(request, env) {
  if (!env.ADMIN_TOKEN || !sameOrigin(request) || !isJson(request)) return unauthorizedJson();
  let body;
  try {
    body = await request.json();
  } catch {
    return unauthorizedJson();
  }
  if (!(await secureEqual(String(body.token || ""), env.ADMIN_TOKEN))) return unauthorizedJson();

  return Response.json(
    { ok: true },
    {
      headers: {
        "Set-Cookie": await sessionCookie(env.ADMIN_TOKEN),
        "Cache-Control": "no-store",
      },
    },
  );
}

export async function hasAdminSession(request, env, nowSeconds = Math.floor(Date.now() / 1000)) {
  if (!env.ADMIN_TOKEN) return false;
  const cookie = request.headers.get("Cookie") || "";
  const actual = cookie
    .split(";")
    .map((part) => part.trim())
    .find((part) => part.startsWith("ceiling_admin="))
    ?.slice("ceiling_admin=".length);
  if (!actual) return false;
  const separator = actual.indexOf(".");
  if (separator <= 0 || separator !== actual.lastIndexOf(".")) return false;
  const expiresText = actual.slice(0, separator);
  if (!/^\d+$/.test(expiresText)) return false;
  const expiresAt = Number(expiresText);
  if (!Number.isSafeInteger(expiresAt) || expiresAt <= nowSeconds) return false;
  return secureEqual(actual, await sessionValue(env.ADMIN_TOKEN, expiresAt));
}

async function metricsResponse(env, ctx, forceRefresh) {
  const cache = caches.default;
  const cacheKey = new Request("https://ceiling.internal/admin-metrics-v1");
  if (!forceRefresh) {
    const cached = await cache.match(cacheKey);
    if (cached) {
      return new Response(cached.body, {
        status: cached.status,
        headers: privateJsonHeaders(),
      });
    }
  }

  const [github, website] = await Promise.all([getGitHubMetrics(env), getWebsiteMetrics(env)]);
  const payload = JSON.stringify({ generatedAt: new Date().toISOString(), github, website });
  ctx.waitUntil(cache.put(cacheKey, new Response(payload, {
    headers: { "Content-Type": "application/json", "Cache-Control": `max-age=${CACHE_SECONDS}` },
  })));
  return new Response(payload, { headers: privateJsonHeaders() });
}

async function getGitHubMetrics(env) {
  const repo = env.GITHUB_REPO || REPO;
  const token = env.GITHUB_TOKEN;
  const headers = githubHeaders(token);
  const base = `https://api.github.com/repos/${repo}`;

  try {
    const [repository, releases] = await Promise.all([
      fetchJson(base, { headers }),
      fetchAllReleases(`${base}/releases?per_page=100`, headers),
    ]);
    const downloads = aggregateReleases(releases);
    const traffic = token ? await getGitHubTraffic(base, headers) : unavailableTraffic("Add GITHUB_TOKEN to unlock repository traffic.");
    return {
      available: true,
      repository: {
        name: repository.full_name,
        stars: numberValue(repository.stargazers_count),
        forks: numberValue(repository.forks_count),
        openIssuesAndPullRequests: numberValue(repository.open_issues_count),
        subscribers: numberValue(repository.subscribers_count),
      },
      downloads,
      traffic,
    };
  } catch (error) {
    return {
      available: false,
      error: friendlyError(error),
      repository: {},
      downloads: emptyDownloads(),
      traffic: unavailableTraffic("GitHub data is currently unavailable."),
    };
  }
}

async function fetchAllReleases(firstUrl, headers) {
  const releases = [];
  let url = firstUrl;
  for (let page = 0; page < 5 && url; page += 1) {
    const response = await fetch(url, { headers });
    if (!response.ok) throw new Error(`GitHub releases returned ${response.status}`);
    const rows = await response.json();
    releases.push(...rows);
    url = nextLink(response.headers.get("Link"));
  }
  return releases;
}

async function getGitHubTraffic(base, headers) {
  const endpoints = [
    ["views", `${base}/traffic/views`],
    ["clones", `${base}/traffic/clones`],
    ["referrers", `${base}/traffic/popular/referrers`],
    ["paths", `${base}/traffic/popular/paths`],
  ];
  const results = await Promise.allSettled(endpoints.map(([, url]) => fetchJson(url, { headers })));
  if (results[0].status !== "fulfilled") {
    return unavailableTraffic("GITHUB_TOKEN needs Administration: Read access to this repository.");
  }
  const views = results[0].value;
  const clones = results[1].status === "fulfilled" ? results[1].value : {};
  const referrers = results[2].status === "fulfilled" ? results[2].value : [];
  const paths = results[3].status === "fulfilled" ? results[3].value : [];
  return {
    available: true,
    views: numberValue(views.count),
    uniqueVisitors: numberValue(views.uniques),
    clones: numberValue(clones.count),
    uniqueCloners: numberValue(clones.uniques),
    days: normalizeGitHubDays(views.views),
    referrers: referrers.slice(0, 10).map((row) => ({
      label: row.referrer || "Unknown",
      views: numberValue(row.count),
      uniques: numberValue(row.uniques),
    })),
    paths: paths.slice(0, 10).map((row) => ({
      label: row.title || row.path || "Unknown",
      path: row.path || "",
      views: numberValue(row.count),
      uniques: numberValue(row.uniques),
    })),
  };
}

async function getWebsiteMetrics(env) {
  if (!env.POSTHOG_QUERY_KEY || !env.POSTHOG_PROJECT_ID) {
    return unavailableWebsite("Add POSTHOG_QUERY_KEY and POSTHOG_PROJECT_ID to unlock website analytics.");
  }

  const conditions = "event = '$pageview' AND properties.$host = 'ceiling.win' AND timestamp >= now() - INTERVAL 30 DAY";
  try {
    const [dailyRows, totalRows, referrerRows, pathRows, clickRows] = await Promise.all([
      hogql(env, `SELECT toString(toDate(timestamp)) AS day, count() AS views, count(DISTINCT person_id) AS uniques FROM events WHERE ${conditions} GROUP BY day ORDER BY day`),
      hogql(env, `SELECT count(DISTINCT person_id), count() FROM events WHERE ${conditions}`),
      hogql(env, `SELECT properties.$referring_domain AS ref, count() AS views FROM events WHERE ${conditions} AND ref IS NOT NULL AND ref != '' AND ref != '$direct' AND ref NOT ILIKE '%ceiling.win%' GROUP BY ref ORDER BY views DESC LIMIT 10`),
      hogql(env, `SELECT properties.$pathname AS path, count() AS views, count(DISTINCT person_id) AS uniques FROM events WHERE ${conditions} GROUP BY path ORDER BY views DESC LIMIT 10`),
      hogql(env, "SELECT count(), count(DISTINCT person_id) FROM events WHERE event = 'download_clicked' AND properties.$host = 'ceiling.win' AND timestamp >= now() - INTERVAL 30 DAY"),
    ]);
    const days = fillWebsiteDays(dailyRows);
    const totals = totalRows[0] || [];
    const clicks = clickRows[0] || [];
    return {
      available: true,
      uniqueVisitors: numberValue(totals[0]),
      views: numberValue(totals[1]),
      downloadClicks: numberValue(clicks[0]),
      downloadClickers: numberValue(clicks[1]),
      days,
      referrers: referrerRows.map((row) => ({ label: String(row[0] || "Direct"), views: numberValue(row[1]) })),
      paths: pathRows.map((row) => ({ label: String(row[0] || "/"), views: numberValue(row[1]), uniques: numberValue(row[2]) })),
    };
  } catch (error) {
    return unavailableWebsite(friendlyError(error));
  }
}

async function hogql(env, query) {
  const host = (env.POSTHOG_QUERY_HOST || "https://us.posthog.com").replace(/\/$/, "");
  const url = `${host}/api/projects/${encodeURIComponent(env.POSTHOG_PROJECT_ID)}/query`;
  const response = await fetch(url, {
    method: "POST",
    headers: {
      Authorization: `Bearer ${env.POSTHOG_QUERY_KEY}`,
      "Content-Type": "application/json",
    },
    body: JSON.stringify({ query: { kind: "HogQLQuery", query } }),
  });
  if (!response.ok) throw new Error(`PostHog query returned ${response.status}`);
  const body = await response.json();
  return Array.isArray(body.results) ? body.results : [];
}

export function aggregateReleases(releases) {
  const rows = [];
  let total = 0;
  let installer = 0;
  let portable = 0;
  for (const release of releases) {
    let releaseTotal = 0;
    const assets = [];
    for (const asset of release.assets || []) {
      const name = String(asset.name || "");
      if (!isDownloadAsset(name)) continue;
      const downloads = numberValue(asset.download_count);
      releaseTotal += downloads;
      total += downloads;
      if (/setup\.exe$/i.test(name)) installer += downloads;
      if (/portable\.exe$/i.test(name)) portable += downloads;
      assets.push({ name, downloads });
    }
    if (assets.length) {
      rows.push({
        tag: release.tag_name || "Unversioned",
        name: release.name || release.tag_name || "Release",
        publishedAt: release.published_at || null,
        downloads: releaseTotal,
        assets,
      });
    }
  }
  return {
    total,
    installer,
    portable,
    latest: rows[0]?.downloads || 0,
    latestTag: rows[0]?.tag || null,
    releases: rows.slice(0, 20),
  };
}

function isDownloadAsset(name) {
  return /\.exe$/i.test(name) && !/\.sha256$/i.test(name);
}

function fillWebsiteDays(rows) {
  const lookup = new Map(rows.map((row) => [String(row[0]), { views: numberValue(row[1]), uniques: numberValue(row[2]) }]));
  const days = [];
  const now = new Date();
  for (let offset = 29; offset >= 0; offset -= 1) {
    const date = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() - offset));
    const day = date.toISOString().slice(0, 10);
    days.push({ day, ...(lookup.get(day) || { views: 0, uniques: 0 }) });
  }
  return days;
}

function normalizeGitHubDays(rows) {
  return (rows || []).map((row) => ({
    day: String(row.timestamp || "").slice(0, 10),
    views: numberValue(row.count),
    uniques: numberValue(row.uniques),
  }));
}

function unavailableWebsite(message) {
  return { available: false, message, uniqueVisitors: 0, views: 0, downloadClicks: 0, downloadClickers: 0, days: [], referrers: [], paths: [] };
}

function unavailableTraffic(message) {
  return { available: false, message, views: 0, uniqueVisitors: 0, clones: 0, uniqueCloners: 0, days: [], referrers: [], paths: [] };
}

function emptyDownloads() {
  return { total: 0, installer: 0, portable: 0, latest: 0, latestTag: null, releases: [] };
}

function githubHeaders(token) {
  const headers = {
    Accept: "application/vnd.github+json",
    "User-Agent": "ceiling.win-analytics",
    "X-GitHub-Api-Version": "2022-11-28",
  };
  if (token) headers.Authorization = `Bearer ${token}`;
  return headers;
}

async function fetchJson(url, init) {
  const response = await fetch(url, init);
  if (!response.ok) throw new Error(`${new URL(url).hostname} returned ${response.status}`);
  return response.json();
}

function nextLink(value) {
  if (!value) return null;
  const match = value.match(/<([^>]+)>;\s*rel="next"/);
  return match?.[1] || null;
}

function numberValue(value) {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

function friendlyError(error) {
  return error instanceof Error ? error.message.slice(0, 160) : "Analytics source unavailable.";
}

function isJson(request) {
  return (request.headers.get("Content-Type") || "").toLowerCase().startsWith("application/json");
}

function sameOrigin(request) {
  const origin = request.headers.get("Origin");
  return !origin || origin === new URL(request.url).origin;
}

export function safePath(value) {
  if (typeof value !== "string" || !value.startsWith("/")) return "/";
  return value.replace(/[\r\n]/g, "").slice(0, 300);
}

function safeLabel(value, maxLength) {
  return typeof value === "string" ? value.replace(/[\r\n]/g, "").slice(0, maxLength) : "";
}

function safeReferrer(value, ownOrigin) {
  if (typeof value !== "string" || !value) return { url: "", domain: "$direct" };
  try {
    const url = new URL(value);
    if (!/^https?:$/.test(url.protocol)) return { url: "", domain: "$direct" };
    return { url: url.origin === ownOrigin ? url.origin + url.pathname : url.origin, domain: url.hostname };
  } catch {
    return { url: "", domain: "$direct" };
  }
}

export async function anonymousVisitorId(request, salt, nowMs = Date.now()) {
  const ip = request.headers.get("CF-Connecting-IP") || "unknown";
  const agent = request.headers.get("User-Agent") || "unknown";
  const rotationPeriod = Math.floor(nowMs / VISITOR_ID_ROTATION_MS);
  return sha256Hex(`${salt}|${rotationPeriod}|${ip}|${agent}`);
}

async function sessionValue(token, expiresAt) {
  const signature = await hmacHex(token, `${SESSION_MESSAGE}:${expiresAt}`);
  return `${expiresAt}.${signature}`;
}

export async function sessionCookie(token, nowSeconds = Math.floor(Date.now() / 1000)) {
  const expiresAt = nowSeconds + SESSION_MAX_AGE;
  return `ceiling_admin=${await sessionValue(token, expiresAt)}; Path=/admin; Max-Age=${SESSION_MAX_AGE}; HttpOnly; Secure; SameSite=Strict`;
}

function expiredSessionCookie() {
  return "ceiling_admin=; Path=/admin; Max-Age=0; HttpOnly; Secure; SameSite=Strict";
}

async function hmacHex(secret, value) {
  const key = await crypto.subtle.importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  const signature = await crypto.subtle.sign("HMAC", key, new TextEncoder().encode(value));
  return bytesToHex(new Uint8Array(signature));
}

async function sha256Hex(value) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return bytesToHex(new Uint8Array(digest));
}

function bytesToHex(bytes) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}

async function secureEqual(left, right) {
  const [leftHash, rightHash] = await Promise.all([sha256Hex(left), sha256Hex(right)]);
  let difference = leftHash.length ^ rightHash.length;
  for (let index = 0; index < Math.max(leftHash.length, rightHash.length); index += 1) {
    difference |= (leftHash.charCodeAt(index) || 0) ^ (rightHash.charCodeAt(index) || 0);
  }
  return difference === 0;
}

function unauthorizedJson() {
  return Response.json({ error: "Unauthorized" }, { status: 401, headers: { "Cache-Control": "no-store" } });
}

function methodNotAllowed() {
  return new Response("Method not allowed", { status: 405 });
}

function privateJsonHeaders() {
  return {
    "Content-Type": "application/json; charset=utf-8",
    "Cache-Control": "private, no-store",
    "X-Content-Type-Options": "nosniff",
  };
}

async function secureAdminResponse(response) {
  const headers = new Headers(response.headers);
  headers.set("Content-Type", "text/html; charset=utf-8");
  headers.set("Cache-Control", "private, no-store");
  headers.set("Content-Security-Policy", "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'");
  headers.set("X-Robots-Tag", "noindex, nofollow");
  headers.set("X-Frame-Options", "DENY");
  headers.set("Referrer-Policy", "no-referrer");
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

function adminLoginPage(configured) {
  const detail = configured
    ? "Enter the private admin token for ceiling.win."
    : "Admin access is disabled until ADMIN_TOKEN is configured.";
  return new Response(`<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><meta name="robots" content="noindex,nofollow"><title>Ceiling analytics</title><style>
    :root{color-scheme:dark;font-family:"Segoe UI Variable Text","Segoe UI",sans-serif}*{box-sizing:border-box}body{margin:0;min-height:100svh;display:grid;place-items:center;background:#060708;color:#e9ebe8}.card{width:min(400px,calc(100vw - 32px));padding:30px;border:1px solid #292d30;border-radius:18px;background:#111416;box-shadow:0 28px 80px #000}.mark{width:28px;height:28px;display:grid;place-items:center;border-radius:8px;background:#192124;color:#53d8d0;font-size:14px}h1{margin:20px 0 7px;font-size:24px;letter-spacing:-.04em}p{margin:0 0 24px;color:#8d9498;font-size:14px;line-height:1.5}form{display:grid;gap:12px}input,button{width:100%;height:45px;border-radius:9px;font:inherit}input{border:1px solid #303538;background:#090b0c;color:#fff;padding:0 13px;outline:none}input:focus{border-color:#58c9c4;box-shadow:0 0 0 3px #58c9c41b}button{border:0;background:#e9ebe8;color:#090a0b;font-weight:650;cursor:pointer}button:disabled{opacity:.45;cursor:default}.error{min-height:18px;margin:2px 0 0;color:#f28181;font-size:12px}</style></head><body><main class="card"><div class="mark">▰</div><h1>Ceiling analytics</h1><p>${detail}</p><form id="login"><input id="token" type="password" autocomplete="current-password" placeholder="Admin token" aria-label="Admin token" ${configured ? "" : "disabled"}><button ${configured ? "" : "disabled"}>Open dashboard</button><div class="error" id="error"></div></form></main><script>document.getElementById('login').addEventListener('submit',async(e)=>{e.preventDefault();const button=e.currentTarget.querySelector('button');const error=document.getElementById('error');button.disabled=true;error.textContent='';try{const response=await fetch('/admin/session',{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({token:document.getElementById('token').value})});if(!response.ok)throw new Error('That token was not accepted.');location.replace('/admin');}catch(err){error.textContent=err.message;button.disabled=false;}});</script></body></html>`, {
    status: configured ? 401 : 503,
    headers: {
      "Content-Type": "text/html; charset=utf-8",
      "Cache-Control": "no-store",
      "Content-Security-Policy": "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; form-action 'none'; base-uri 'none'; frame-ancestors 'none'",
      "X-Robots-Tag": "noindex, nofollow",
      "X-Frame-Options": "DENY",
    },
  });
}
