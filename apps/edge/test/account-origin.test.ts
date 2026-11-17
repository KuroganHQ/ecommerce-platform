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
  const shop = site({ theme_artifact: theme.id, account_host: "demo.accounts.localhost" });
  gw = createGateway({
    artifactRoot: root,
    resolver: new StaticResolver({
      "demo.localhost": shop,
      "demo.accounts.localhost": { ...shop, kind: "account" },
      "so.localhost": {
        ...shop,
        shop_host: "so.localhost",
        account_host: "sk--demo.accounts.localhost",
        checkout_origins: [
          {
            origin: "http://so.localhost:8280",
            mode: "same_origin",
            mode_generation: 1,
            checkout_origin: "http://so.localhost:8280",
          },
        ],
      },
      "noacc.localhost": { ...shop, shop_host: "noacc.localhost", account_host: undefined },
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
      "/identify/x",
      "/placement/",
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

    await post(`/_p/account/orders/${ORDER}/withdrawal`, {
      ...sameOrigin,
      origin: ACC,
      cookie: SESSION,
    });
    expect(api.calls.at(-1)).toMatchObject({
      url: `http://api.test/storefront/v1/customer/orders/${ORDER}/withdrawal`,
      headers: { "x-customer-session": "sessiontoken_000000000001" },
    });
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

const SO = "http://so.localhost:8280";
const STATE = "state_0123456789abcdef";
const CODE = "c".repeat(64);
const a2 = (path: string, origin: string, extra = "") =>
  `${ACC}${path}?state=${STATE}&origin=${encodeURIComponent(origin)}${extra}`;

const expectSeeOther = async (res: Response, location: string) => {
  expect(res.status, location).toBe(303);
  expect(res.headers.get("location")).toBe(location);
  expect(res.headers.get("cache-control")).toBe("no-store");
  expect(res.headers.get("referrer-policy")).toBe("no-referrer");
  expect(await res.text()).toBe("");
};

describe("A2 account half (SHOP-95 slice 3)", () => {
  test("identify_rejects_unregistered_origin", async () => {
    for (const path of ["/identify", "/placement"]) {
      api.calls.length = 0;
      const res = await get(a2(path, "http://evil.example"), { cookie: SESSION });
      expect(res.status, path).toBe(400);
      expect(res.headers.get("location"), path).toBeNull();
      expect(res.headers.get("cache-control"), path).toBe("no-store");
      const sent = api.calls.at(-1);
      expect(sent?.url).toBe("http://api.test/storefront/v1/customer/account-codes");
      expect(JSON.parse(sent?.body ?? "{}")).toEqual({
        purpose: path.slice(1),
        state: STATE,
        origin: "http://evil.example",
      });
      expect(sent?.headers["x-customer-session"]).toBe("sessiontoken_000000000001");
      const missing = await get(`${ACC}${path}?state=${STATE}`, { cookie: SESSION });
      expect(missing.status, path).toBe(400);
      expect(JSON.parse(api.calls.at(-1)?.body ?? "{}").origin).toBe("");
    }
    for (const method of ["POST", "PUT"])
      expect((await get(a2("/identify", SO), { cookie: SESSION }, { method })).status).toBe(405);
  });

  test("identify_redirect_target_is_fixed_path", async () => {
    const tricks =
      "&next=%2F%2Fevil.example&redirect_uri=https%3A%2F%2Fevil.example&callback=http%3A%2F%2Fevil.example&path=%2Fx";
    await expectSeeOther(
      await get(a2("/identify", SO, tricks), { cookie: SESSION }),
      `${SO}/checkout/identify?state=${STATE}&code=${CODE}`,
    );
    await expectSeeOther(
      await get(a2("/placement", SO, tricks), { cookie: SESSION }),
      `${SO}/checkout/placement?state=${STATE}&code=${CODE}`,
    );
  });

  test("identify_signs_in_first_and_placement_never_prompts", async () => {
    const next = `/identify?state=${STATE}&origin=${encodeURIComponent(SO)}`;
    await expectSeeOther(
      await get(a2("/identify", SO, "&next=%2F%2Fevil.example")),
      `/account?next=${encodeURIComponent(next)}`,
    );
    const res = await get(a2("/placement", SO));
    await expectSeeOther(res, `${SO}/checkout/placement?state=${STATE}&code=${CODE}`);
    expect(api.calls.at(-1)?.headers["x-customer-session"]).toBeUndefined();
  });
});

const CO = "http://checkout.demo.localhost:8280";
const LEGACY = [
  "/account",
  "/account/addresses",
  "/account/security",
  `/account/orders/${ORDER}`,
  `/account/verify?token=${"a".repeat(64)}&next=%2Faccount`,
];

const expectRedirect = async (res: Response, location: string) => {
  expect(res.status, location).toBe(308);
  expect(res.headers.get("location")).toBe(location);
  expect(res.headers.get("cache-control")).toBe("no-store");
  expect(await res.text()).toBe("");
};

describe("legacy account routes (SHOP-95 slice 2)", () => {
  test("checkout_host_account_paths_redirect_to_account_origin", async () => {
    api.calls.length = 0;
    for (const p of LEGACY)
      await expectRedirect(await get(`${CO}${p}`, { cookie: SESSION }), `${ACC}${p}`);
    expect(api.calls).toEqual([]);
    for (const p of ["/_p/account/me", "/_p/account/login", "/_p/account/consent"]) {
      expect((await get(`${CO}${p}`, { ...sameOrigin, cookie: SESSION })).status, p).toBe(404);
      const body = "{}";
      const headers = { ...sameOrigin, origin: CO, "content-type": "application/json" };
      expect((await get(`${CO}${p}`, headers, { method: "POST", body })).status, p).toBe(404);
    }
    expect(api.calls).toEqual([]);
  });

  test("shop_origin_account_paths_redirect", async () => {
    for (const p of LEGACY) await expectRedirect(await get(`${SHOP}${p}`), `${ACC}${p}`);
    const so = "http://so.localhost:8280";
    const other = "http://sk--demo.accounts.localhost:8280";
    for (const p of LEGACY) await expectRedirect(await get(`${so}${p}`), `${other}${p}`);
    for (const p of ["/cs/account", "/account/", "/account/other", "/accounts"])
      expect((await get(`${SHOP}${p}`)).status, p).toBe(404);
    for (const base of ["http://noacc.localhost:8280", "http://checkout.noacc.localhost:8280"])
      expect((await get(`${base}/account`)).status, base).toBe(404);
  });

  test("legacy_redirect_ignores_raw_path", async () => {
    for (const base of [SHOP, CO]) {
      for (const p of [
        "//attacker.example/account",
        "/account//attacker.example",
        "/account/%2F%2Fattacker.example",
        "/account%2F..%2F..%2Fattacker.example",
        "/account%2faddresses",
        "/%61ccount",
        "/account/./security/../addresses/..%2F",
        "/Account",
        "/account/orders/..%2F..%2Fattacker.example",
        "/account\\attacker.example",
      ]) {
        const res = await get(`${base}${p}`);
        const location = res.headers.get("location");
        if (location !== null) expect(new URL(location).origin, `${base}${p}`).toBe(ACC);
        if (location !== null) expect(LEGACY.map((l) => `${ACC}${l}`)).toContain(location);
      }
    }
  });

  test("checkout_never_forwards_session", async () => {
    const cookie = `${SESSION}; __Host-cart=checkouttoken_000000000001; __Secure-consent_id=${"a".repeat(32)}`;
    const attempt = "0199a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
    const json = { origin: CO, "content-type": "application/json", cookie };
    api.calls.length = 0;
    await get(`${CO}/`, { cookie });
    await get(`${CO}/consent`, { cookie });
    await get(`${CO}/_p/checkout`, { cookie });
    await get(`${CO}/_p/checkout/place-order`, json, { method: "POST", body: "{}" });
    await get(`${CO}/_p/orders/${"a".repeat(64)}`, { cookie });
    await get(`${CO}/_p/withdraw`, json, { method: "POST", body: "{}" });
    await get(`${CO}/_p/fake-pay/${attempt}`, { cookie });
    await get(
      `${CO}/_p/fake-pay/${attempt}`,
      { ...json, "content-type": "application/x-www-form-urlencoded" },
      {
        method: "POST",
        body: "outcome=succeeded",
      },
    );
    await get(`${CO}/_p/consent`, { cookie });
    await get(`${CO}/_p/consent`, json, { method: "POST", body: "{}" });
    for (const path of ["/checkout", "/orders/", "/withdrawals", "/checkout/fake-pay/", "/consent"])
      expect(
        api.calls.some((c) => c.url.includes(`/storefront/v1${path}`)),
        path,
      ).toBe(true);
    for (const call of api.calls)
      expect(call.headers["x-customer-session"], `${call.method} ${call.url}`).toBeUndefined();
  });
});
