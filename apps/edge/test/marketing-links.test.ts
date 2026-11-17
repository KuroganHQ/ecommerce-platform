import { createHmac } from "node:crypto";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, fakeApi, site } from "./fixtures.ts";

const SECRET = "test-page-token-secret-0123456789abcdef";
const FRESH = "http://fresh.localhost:8280";
const LEGACY = "http://checkout.fresh.localhost:8280";
const LINK = "5a".repeat(32);
const GRANT = "5b".repeat(32);
const OLD = "5c".repeat(32);
const RESTORED = "5d".repeat(32);
const CHECKOUT = "5e".repeat(32);
const tokenOf = (capability: string) =>
  createHmac("sha256", SECRET).update(`pt1|fresh.localhost:8280|${capability}`).digest("base64url");
const NAVIGATE = {
  "sec-fetch-site": "cross-site",
  "sec-fetch-mode": "navigate",
  "sec-fetch-dest": "document",
};
const FORM = {
  origin: FRESH,
  "sec-fetch-site": "same-origin",
  "sec-fetch-mode": "navigate",
  "sec-fetch-dest": "document",
  "content-type": "application/x-www-form-urlencoded",
};
const REFUSED: [string, Record<string, string>][] = [
  ["prefetch", { ...NAVIGATE, "sec-purpose": "prefetch" }],
  ["prerender", { ...NAVIGATE, "sec-purpose": "prefetch;prerender" }],
  ["fetch", { "sec-fetch-site": "same-origin", "sec-fetch-mode": "cors", "sec-fetch-dest": "empty" }],
  ["iframe", { ...NAVIGATE, "sec-fetch-dest": "iframe" }],
];

const LINKS: [string, string, string][] = [
  ["/newsletter/confirm?token=", "newsletter_confirmation", "/newsletter/confirm"],
  ["/newsletter?t=", "newsletter_preferences", "/newsletter"],
  ["/watch/confirm?token=", "watch_confirmation", "/watch/confirm"],
  ["/watch/unsubscribe?token=", "watch_unsubscribe", "/watch/unsubscribe"],
  ["/flows/unsubscribe?token=", "flow_unsubscribe", "/flows/unsubscribe"],
  ["/restore-cart?token=", "restore_cart", "/restore-cart"],
];

const FORMS: [string, string, string, string][] = [
  [
    "/newsletter/confirm",
    "/link-grants/newsletter-confirmation",
    "/newsletter/confirm?done=1",
    "/newsletter/confirm",
  ],
  [
    "/newsletter/unsubscribe",
    "/link-grants/newsletter-preferences/unsubscribe",
    "/newsletter?done=unsubscribed",
    "/newsletter",
  ],
  [
    "/newsletter/resubscribe",
    "/link-grants/newsletter-preferences/resubscribe",
    "/newsletter?done=resubscribed",
    "/newsletter",
  ],
  ["/watch/confirm", "/link-grants/watch-confirmation", "/watch/confirm?done=1", "/watch/confirm"],
  [
    "/watch/unsubscribe",
    "/link-grants/watch-unsubscribe",
    "/watch/unsubscribed",
    "/watch/unsubscribe",
  ],
  [
    "/flows/unsubscribe",
    "/link-grants/flow-unsubscribe",
    "/flows/unsubscribed",
    "/flows/unsubscribe",
  ],
];

const grantPageWorker = `
export default {
  async fetch(request) {
    const token = request.headers.get("x-platform-grant-page-token");
    return new Response("<!doctype html><meta name=\\"page-token\\" content=\\"" + (token ?? "") + "\\">", {
      headers: { "content-type": "text/html" },
    });
  },
};
`;

type Call = { method: string; path: string; headers: Record<string, string>; body: string };
let calls: Call[];
let gw: Gateway;
let actionStatus: number;

function api() {
  const base = fakeApi();
  return async (req: Request): Promise<Response> => {
    const url = new URL(req.url);
    const body = await req.clone().text();
    const p = url.pathname.replace("/storefront/v1", "");
    calls.push({ method: req.method, path: p, headers: Object.fromEntries(req.headers), body });
    if (p === "/link-grants" && req.method === "POST") {
      const link = (JSON.parse(body) as { link: { kind: string; token: string } }).link;
      if (link.token !== LINK) return Response.json({ code: "not_found" }, { status: 404 });
      return Response.json(
        { resource: { kind: link.kind } },
        { status: 201, headers: { "x-link-grant": GRANT } },
      );
    }
    if (p === "/link-grants/restore-cart")
      return actionStatus === 200
        ? Response.json({ cart_token: RESTORED })
        : new Response(null, { status: actionStatus });
    if (p.startsWith("/link-grants/")) return new Response(null, { status: actionStatus });
    if (p === "/cart/checkout") return Response.json({ cart_token: CHECKOUT });
    if (p === "/unsubscribe" || p === "/newsletter/unsubscribe")
      return Response.json({ status: "unsubscribed" }, { status: actionStatus });
    if (p === "/newsletter/click")
      return url.searchParams.get("s") === "good"
        ? Response.json({ url: "https://brand.example/p/kabat" })
        : Response.json({ code: "not_found" }, { status: 404 });
    return base.fn(req);
  };
}

const send = (
  url: string,
  headers: Record<string, string> = {},
  init: { method?: string; body?: string } = {},
) => gw.fetch(new Request(url, { ...init, headers: { host: new URL(url).host, ...headers } }));

const post = (pathname: string, body: string, headers: Record<string, string> = {}) =>
  send(`${FRESH}${pathname}`, { ...FORM, ...headers }, { method: "POST", body });

const apiPaths = () => calls.map((c) => c.path);

beforeAll(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "shop139-"));
  const checkout = await buildArtifact(root, "checkout", grantPageWorker, {
    "_checkout/0123456789abcdef/app.js": "1",
  });
  gw = createGateway({
    artifactRoot: root,
    resolver: new StaticResolver({
      "fresh.localhost": site({
        tenant_id: "t-fresh",
        shop_host: "fresh.localhost",
        checkout_origins: [
          { origin: FRESH, mode: "same_origin", mode_generation: 1, checkout_origin: FRESH },
        ],
      }),
    }),
    checkoutArtifact: checkout.id,
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream: api(),
    pageTokenSecret: SECRET,
  });
});

beforeEach(() => {
  calls = [];
  actionStatus = 200;
});

afterAll(async () => {
  await gw?.dispose();
});

describe("newsletter, watch, flow and restore links exchange before render (C5)", () => {
  test.each(LINKS)("%s answers 303 to %s's clean page with the grant", async (prefix, kind, clean) => {
    const res = await send(`${FRESH}${prefix}${LINK}`, {
      ...NAVIGATE,
      cookie: `__Host-Http-cap=${OLD}`,
    });
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(clean);
    expect(res.headers.get("set-cookie")).toBe(
      `__Host-Http-cap=${GRANT}; Path=/; HttpOnly; Secure; SameSite=Lax`,
    );
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(calls.map((c) => JSON.parse(c.body))).toEqual([
      { link: { kind, token: LINK }, previous: OLD },
    ]);
  });

  test.each(LINKS)("%s fails closed and keeps the browser's grant", async (prefix, _kind, clean) => {
    for (const bad of [`${prefix}${"0".repeat(64)}`, `${prefix}xyz`, `${prefix}${LINK}&t=${LINK}&token=${LINK}`]) {
      const res = await send(`${FRESH}${bad}`, { ...NAVIGATE, cookie: `__Host-Http-cap=${OLD}` });
      expect(res.status, bad).toBe(303);
      expect(res.headers.get("location"), bad).toBe(`${clean}?link=invalid`);
      expect(res.headers.get("set-cookie"), bad).toBeNull();
    }
  });

  const routes = LINKS.flatMap(([prefix]) => REFUSED.map(([how, h]) => [prefix, how, h] as const));
  test.each(routes)("%s refuses a %s request without calling the API", async (prefix, _how, h) => {
    const res = await send(`${FRESH}${prefix}${LINK}`, h);
    expect(res.status).toBe(403);
    expect(await res.text()).toBe("");
    expect(calls).toEqual([]);
  });

  test("a clean page renders the grant's page token and no link", async () => {
    const res = await send(`${FRESH}/newsletter`, { ...NAVIGATE, cookie: `__Host-Http-cap=${GRANT}` });
    expect(res.status).toBe(200);
    const html = await res.text();
    expect(html).toContain(tokenOf(GRANT));
    expect(html).not.toContain(GRANT);
  });
});

describe("confirmation, preference and unsubscribe forms use the grant and _pt (C4)", () => {
  test.each(FORMS)("POST %s calls %s with the grant only", async (form, apiPath, done) => {
    const res = await post(form, `_pt=${tokenOf(GRANT)}`, {
      cookie: `__Host-Http-cap=${GRANT}; __Host-Http-cart=${OLD}; cart=${OLD}`,
    });
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(done);
    expect(apiPaths()).toEqual([apiPath]);
    expect(calls[0]?.headers["x-link-grant"]).toBe(GRANT);
    expect(calls[0]?.headers["x-cart-token"]).toBeUndefined();
    expect(calls[0]?.body).toBe("");
  });

  test.each(FORMS)("POST %s with a stale, missing or doubled _pt reloads its page", async (form, _a, _d, page) => {
    for (const body of [`_pt=${tokenOf(OLD)}`, "", `_pt=${tokenOf(GRANT)}&_pt=${tokenOf(GRANT)}`]) {
      const res = await post(form, body, { cookie: `__Host-Http-cap=${GRANT}` });
      expect(res.status, body).toBe(303);
      expect(res.headers.get("location"), body).toBe(page);
    }
    expect(calls).toEqual([]);
  });

  test.each(FORMS)("POST %s answers a spent grant with the invalid-link state", async (form, _a, _d, page) => {
    actionStatus = 404;
    const res = await post(form, `_pt=${tokenOf(GRANT)}`, { cookie: `__Host-Http-cap=${GRANT}` });
    expect(res.headers.get("location")).toBe(`${page}?link=invalid`);
  });

  test.each(FORMS)("POST %s from a script or another site is refused", async (form) => {
    for (const h of [
      { "sec-fetch-mode": "cors", "sec-fetch-dest": "empty" },
      { origin: "http://evil.example", "sec-fetch-site": "cross-site" },
    ]) {
      const res = await post(form, `_pt=${tokenOf(GRANT)}`, { cookie: `__Host-Http-cap=${GRANT}`, ...h });
      expect(res.status).toBe(403);
    }
    expect(calls).toEqual([]);
  });

  test.each(["/newsletter/unsubscribe", "/newsletter/resubscribe"])(
    "GET %s changes nothing",
    async (pathname) => {
      const res = await send(`${FRESH}${pathname}`, { ...NAVIGATE, cookie: `__Host-Http-cap=${GRANT}` });
      expect(res.status).toBe(404);
      expect(calls).toEqual([]);
    },
  );

  test("restore makes a shop cart and starts checkout from it", async () => {
    const res = await post("/restore-cart", `_pt=${tokenOf(GRANT)}`, {
      cookie: `__Host-Http-cap=${GRANT}; __Host-Http-cart=${OLD}`,
    });
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("/checkout");
    expect(res.headers.getSetCookie()).toEqual([
      `__Host-Http-cart=${CHECKOUT}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=86400`,
      `cart=${RESTORED}; Path=/_p; HttpOnly; Secure; SameSite=Lax; Max-Age=2592000`,
    ]);
    expect(apiPaths()).toEqual(["/link-grants/restore-cart", "/cart/checkout"]);
    expect(calls[1]?.headers["x-cart-token"]).toBe(RESTORED);
    expect(JSON.parse(calls[1]?.body ?? "")).toEqual({ previous: OLD });
  });
});

describe("no blanket exemption under /_p on a same-origin shop", () => {
  test.each([
    "/_p/newsletter/confirm",
    "/_p/newsletter/unsubscribe",
    "/_p/newsletter/resubscribe",
    "/_p/newsletter/click",
    "/_p/watch/confirm",
    "/_p/flows/restore-cart",
    "/_p/flows/unsubscribe",
  ])("%s reaches no API", async (pathname) => {
    const res = await send(
      `${FRESH}${pathname}?t=${LINK}&token=${LINK}`,
      { ...FORM, cookie: `__Host-Http-cap=${GRANT}` },
      { method: "POST", body: `token=${LINK}&t=${LINK}&_pt=${tokenOf(GRANT)}` },
    );
    expect(res.status).toBe(404);
    expect(calls).toEqual([]);
  });

  test("public sign-up stays on POST /_p/newsletter and /_p/watch (default 13)", async () => {
    const json = { ...FORM, "sec-fetch-mode": "cors", "content-type": "application/json" };
    await send(`${FRESH}/_p/newsletter`, json, { method: "POST", body: '{"email":"a@b.cz"}' });
    await send(`${FRESH}/_p/watch`, json, { method: "POST", body: '{"email":"a@b.cz"}' });
    expect(apiPaths()).toEqual(["/newsletter/subscribe", "/watch/subscribe"]);
  });
});

describe("RFC 8058 one-click (C11)", () => {
  const ONE_CLICK = `${FRESH}/_p/unsubscribe/${LINK}`;
  const body = "List-Unsubscribe=One-Click";
  const machine = { "content-type": "application/x-www-form-urlencoded" };

  test("one_click_without_origin_or_cookie unsubscribes by the bearer alone", async () => {
    const res = await send(ONE_CLICK, machine, { method: "POST", body });
    expect(res.status).toBe(200);
    expect(apiPaths()).toEqual(["/unsubscribe"]);
    expect(JSON.parse(calls[0]?.body ?? "")).toEqual({ token: LINK });
    expect(res.headers.get("set-cookie")).toBeNull();
    expect(await res.text()).toBe("Unsubscribed");
  });

  test("one_click_ignores_attacker_cookie", async () => {
    const res = await send(
      ONE_CLICK,
      {
        ...machine,
        origin: "http://evil.example",
        cookie: `__Host-Http-cap=${OLD}; __Host-Http-cart=${OLD}; cart=${OLD}`,
      },
      { method: "POST", body },
    );
    expect(res.status).toBe(200);
    const headers = calls[0]?.headers ?? {};
    for (const h of ["x-link-grant", "x-cart-token", "cookie", "x-page-token"])
      expect(headers[h], h).toBeUndefined();
    expect(JSON.parse(calls[0]?.body ?? "")).toEqual({ token: LINK });
  });

  test("one_click_get_does_not_unsubscribe", async () => {
    for (const method of ["GET", "HEAD"]) {
      const res = await send(ONE_CLICK, NAVIGATE, { method });
      expect(res.status, method).toBe(405);
    }
    expect(calls).toEqual([]);
  });

  test("an unknown or malformed token answers 404", async () => {
    actionStatus = 404;
    expect((await send(ONE_CLICK, machine, { method: "POST", body })).status).toBe(404);
    expect((await send(`${FRESH}/_p/unsubscribe/xyz`, machine, { method: "POST", body })).status).toBe(404);
    expect(apiPaths()).toEqual(["/unsubscribe"]);
  });

  test("an old checkout-host one-click URL still unsubscribes directly (default 12)", async () => {
    const res = await send(`${LEGACY}/_p/newsletter/unsubscribe?t=${LINK}`, machine, {
      method: "POST",
      body,
    });
    expect(res.status).toBe(200);
    expect(apiPaths()).toEqual(["/newsletter/unsubscribe"]);
  });
});

describe("signed click-through", () => {
  const click = (q: string) => `${FRESH}/newsletter/click?${q}`;

  test("redirects an admitted navigation to the signed target, never revealing the subscriber", async () => {
    const res = await send(click(`t=${LINK}&u=https%3A%2F%2Fbrand.example%2Fp%2Fkabat&s=good`), NAVIGATE);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("https://brand.example/p/kabat");
    expect(res.headers.get("referrer-policy")).toBe("strict-origin");
    expect(res.headers.get("set-cookie")).toBeNull();
    expect(await res.text()).toBe("");
  });

  test.each([
    ["a bad signature", `t=${LINK}&u=https%3A%2F%2Fevil.example&s=bad`],
    ["a missing signature", `t=${LINK}&u=https%3A%2F%2Fevil.example`],
    ["a doubled target", `t=${LINK}&u=https%3A%2F%2Fbrand.example&u=https%3A%2F%2Fevil.example&s=good`],
    ["an extra parameter", `t=${LINK}&u=https%3A%2F%2Fbrand.example&s=good&next=https%3A%2F%2Fevil.example`],
  ])("%s answers 404 with no Location", async (_what, q) => {
    const res = await send(click(q), NAVIGATE);
    expect(res.status).toBe(404);
    expect(res.headers.get("location")).toBeNull();
  });

  test.each(REFUSED)("a %s request is refused before the API", async (_how, h) => {
    const res = await send(click(`t=${LINK}&u=https%3A%2F%2Fbrand.example&s=good`), h);
    expect(res.status).toBe(403);
    expect(calls).toEqual([]);
  });
});
