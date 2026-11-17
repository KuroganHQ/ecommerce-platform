import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, checkoutWorker, fakeApi, hostileTheme, site } from "./fixtures.ts";

const ACC = "http://demo.accounts.localhost:8280";
const SHOP = "http://demo.localhost:8280";
const SESSION = "__Host-sid=sessiontoken_000000000001";
const ORDER = "12345678-1234-1234-1234-123456789abc";

let gw: Gateway;
let api: ReturnType<typeof fakeApi>;

const get = (url: string, headers: Record<string, string> = {}, init: RequestInit = {}) =>
  gw.fetch(new Request(url, { ...init, headers: { host: new URL(url).host, ...headers } }));

const post = (route: string, headers: Record<string, string>, body = "{}") =>
  get(
    `${ACC}${route}`,
    { "content-type": "application/json", ...headers },
    { method: "POST", body },
  );

const sameOrigin = { "sec-fetch-site": "same-origin" };

beforeAll(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "account-origin-"));
  const theme = await buildArtifact(root, "theme", hostileTheme("v1"), {
    "_astro/app.js": "console.log(1)",
  });
  const checkout = await buildArtifact(root, "checkout", checkoutWorker, {
    "_checkout/0123456789abcdef/checkout.js": "1",
  });
  api = fakeApi();
  const shop = site({ theme_artifact: theme.id });
  gw = createGateway({
    artifactRoot: root,
    resolver: new StaticResolver({
      "demo.localhost": shop,
      "demo.accounts.localhost": { ...shop, kind: "account" },
    }),
    checkoutArtifact: checkout.id,
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream: api.fn,
    renderTimeoutMs: 1500,
  });
});

afterAll(async () => {
  await gw?.dispose();
});

describe("account origin (SHOP-95 slice 1, A1)", () => {
  test("account_host_serves_only_account_routes", async () => {
    const root = await get(`${ACC}/`);
    expect(root.status).toBe(303);
    expect(root.headers.get("location")).toBe("/account");
    for (const p of [
      "/account",
      "/account/addresses",
      "/account/security",
      `/account/verify?token=${"a".repeat(64)}`,
      `/account/orders/${ORDER}`,
      "/account/consent",
    ]) {
      const res = await get(`${ACC}${p}`, { cookie: SESSION });
      expect(res.status, p).toBe(200);
      expect(res.headers.get("content-type"), p).toContain("text/html");
    }
    expect((await get(`${ACC}/_p/tokens.css`)).status).toBe(200);
    expect((await get(`${ACC}/_checkout/0123456789abcdef/checkout.js`)).status).toBe(200);
    for (const p of [
      "/checkout",
      "/consent",
      "/withdraw",
      `/o/${"a".repeat(64)}`,
      "/newsletter",
      "/review",
      "/restore-cart",
      "/start?h=x",
      "/identify",
      "/placement",
      "/account/other",
      "/account/orders/not-a-uuid",
      "/accounts",
      "/robots.txt",
      "/_astro/app.js",
      "/media/t-demo/x.webp",
      "/_p/consent",
      "/_p/cart",
      "/_p/checkout",
      "/_p/public/shop",
      "/_p/e",
      "/_p/account/admin",
    ]) {
      expect((await get(`${ACC}${p}`, sameOrigin)).status, p).toBe(404);
    }
    expect((await get(`${ACC}/account`, {}, { method: "POST", body: "" })).status).toBe(405);
    expect(
      (await get(`${ACC}/account`, { "service-worker": "script" })).status,
      "service workers",
    ).toBe(403);
    expect((await get("http://checkout.demo.accounts.localhost:8280/account")).status).toBe(404);
  });

  test("account pages get only the session; cart and consent cookies stay behind", async () => {
    api.calls.length = 0;
    const page = await get(`${ACC}/account`, {
      cookie: `${SESSION}; __Host-cart=checkouttoken_000000000001; __Secure-consent_id=${"a".repeat(32)}`,
    });
    expect(await page.text()).toContain("session_seen");
    for (const call of api.calls) {
      expect(call.headers["x-cart-token"], call.url).toBeUndefined();
      expect(call.headers["x-consent-subject"], call.url).toBeUndefined();
    }
    expect(
      api.calls.find((c) => c.url.endsWith("/customer/me"))?.headers["x-customer-session"],
    ).toBe("sessiontoken_000000000001");
  });

  test("account_api_requires_same_origin_fetch_metadata", async () => {
    const calls = api.calls.length;
    for (const site of ["same-site", "cross-site", "none"]) {
      expect(
        (await get(`${ACC}/_p/account/me`, { cookie: SESSION, "sec-fetch-site": site })).status,
        site,
      ).toBe(403);
      expect(
        (await post("/_p/account/login", { origin: ACC, "sec-fetch-site": site })).status,
        site,
      ).toBe(403);
    }
    for (const origin of [SHOP, "http://evil.example", "null"]) {
      expect((await post("/_p/account/login", { ...sameOrigin, origin })).status, origin).toBe(403);
    }
    expect(api.calls.length).toBe(calls);

    const me = await get(`${ACC}/_p/account/me`, { ...sameOrigin, cookie: SESSION });
    expect(me.status).toBe(200);
    expect(me.headers.get("set-cookie")).toBe(
      "__Host-sid=sessiontoken_000000000001; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=2592000",
    );

    const login = await post(
      "/_p/account/login",
      {
        ...sameOrigin,
        origin: ACC,
        cookie: `__Host-cart=checkouttoken_000000000001; __Secure-consent_id=${"a".repeat(32)}`,
      },
      JSON.stringify({ email: "jana@example.cz", password: "correct horse battery" }),
    );
    expect(login.status).toBe(200);
    expect(login.headers.get("set-cookie")).toBe(
      "__Host-sid=sessiontoken_000000000001; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=2592000",
    );
    const sent = api.calls.at(-1);
    expect(sent?.url).toBe("http://api.test/storefront/v1/customer/login");
    expect(sent?.headers["x-cart-token"]).toBeUndefined();
    expect(sent?.headers["x-consent-subject"]).toBeUndefined();

    for (const [method, body] of [
      ["GET", undefined],
      ["POST", JSON.stringify({ purposes: { email_marketing: true }, text_version: "v1" })],
    ] as const) {
      await get(
        `${ACC}/_p/account/consent`,
        { ...sameOrigin, cookie: SESSION, "content-type": "application/json" },
        { method, body },
      );
      const call = api.calls.at(-1);
      expect(call?.method).toBe(method);
      expect(call?.url).toBe("http://api.test/storefront/v1/customer/consent");
      expect(call?.headers["x-customer-session"]).toBe("sessiontoken_000000000001");
    }
  });

  test("account_api_rejects_missing_fetch_metadata", async () => {
    const calls = api.calls.length;
    expect((await get(`${ACC}/_p/account/me`, { cookie: SESSION })).status).toBe(403);
    expect((await get(`${ACC}/_p/account/me`, { cookie: SESSION, origin: ACC })).status).toBe(403);
    expect((await post("/_p/account/login", { origin: ACC })).status).toBe(403);
    expect((await post("/_p/account/logout", { origin: ACC, cookie: SESSION })).status).toBe(403);
    expect(api.calls.length).toBe(calls);
  });

  test("account_pages_isolation_headers", async () => {
    for (const p of ["/account", "/account/consent", `/account/verify?token=${"a".repeat(64)}`]) {
      const res = await get(`${ACC}${p}`);
      const csp = res.headers.get("content-security-policy") ?? "";
      expect(res.headers.get("cache-control"), p).toBe("no-store");
      expect(res.headers.get("x-frame-options"), p).toBe("DENY");
      expect(res.headers.get("cross-origin-opener-policy"), p).toBe("same-origin");
      expect(csp, p).toContain("frame-ancestors 'none'");
      expect(csp, p).toContain("worker-src 'none'");
      expect(csp, p).toContain("frame-src 'none'");
      expect(csp, p).not.toContain("stripe");
      expect(csp, p).not.toContain("packeta");
      expect(res.headers.get("referrer-policy"), p).toBe(
        p.startsWith("/account/verify") ? "no-referrer" : "strict-origin",
      );
    }
  });

  test("account_responses_have_no_cors", async () => {
    const responses = [
      await get(`${ACC}/account`, { origin: SHOP }),
      await get(`${ACC}/_p/account/me`, { origin: SHOP, cookie: SESSION }),
      await get(`${ACC}/_p/account/me`, { ...sameOrigin, cookie: SESSION }),
      await get(
        `${ACC}/_p/account/login`,
        { origin: SHOP, "access-control-request-method": "POST" },
        { method: "OPTIONS" },
      ),
      await post("/_p/account/login", { ...sameOrigin, origin: ACC }, "{}"),
    ];
    for (const res of responses) {
      for (const name of res.headers.keys()) expect(name).not.toMatch(/^access-control-/);
    }
  });
});
