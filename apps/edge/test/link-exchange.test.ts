import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { type CheckoutPolicy, StaticResolver } from "../src/sites.ts";
import {
  buildArtifact,
  EVIL_LINK,
  fakeApi,
  hostileTheme,
  LINK_GRANT,
  LINK_THROWS,
  ORDER_ID,
  ORDER_LINK,
  platformCheckoutWorker,
  site,
} from "./fixtures.ts";

const FRESH = "http://fresh.localhost:8280";
const DEMO = "http://demo.localhost:8280";
const OLD_GRANT = "4d".repeat(32);
const CART = "sameorigintoken_00000000000000000042";
const NAVIGATE = {
  "sec-fetch-site": "cross-site",
  "sec-fetch-mode": "navigate",
  "sec-fetch-dest": "document",
  "sec-fetch-user": "?1",
};
const REFUSED: [string, Record<string, string>][] = [
  ["prefetch", { ...NAVIGATE, "sec-purpose": "prefetch" }],
  ["prerender", { ...NAVIGATE, "sec-purpose": "prefetch;prerender" }],
  [
    "fetch",
    { "sec-fetch-site": "same-origin", "sec-fetch-mode": "cors", "sec-fetch-dest": "empty" },
  ],
  [
    "no-cors",
    { "sec-fetch-site": "same-origin", "sec-fetch-mode": "no-cors", "sec-fetch-dest": "image" },
  ],
  ["iframe", { ...NAVIGATE, "sec-fetch-dest": "iframe" }],
];
const C5_PARAMETERS = [
  "token",
  "t",
  "code",
  "payment_intent",
  "payment_intent_client_secret",
  "setup_intent",
  "setup_intent_client_secret",
  "redirect_status",
  "source",
  "client_secret",
];
const SECRET = "c0ffee".repeat(10);
const CLEAN_PAGES = [
  "/checkout",
  `/o/${ORDER_ID}`,
  "/withdraw",
  "/review",
  "/newsletter/preferences",
];

const LINK_POSITIONS = new Map([
  ["/withdraw", "t"],
  ["/review", "token"],
]);

const sameOrigin = (origin: string): CheckoutPolicy => ({
  origin,
  mode: "same_origin",
  mode_generation: 1,
  checkout_origin: origin,
});

let api: ReturnType<typeof fakeApi>;
let gw: Gateway;
const logged: unknown[] = [];

const get = (url: string, headers: Record<string, string> = {}, init: RequestInit = {}) =>
  gw.fetch(new Request(url, { ...init, headers: { host: new URL(url).host, ...headers } }));

const exchanges = () => api.calls.filter((c) => c.url.endsWith("/storefront/v1/link-grants"));

function expectNoSecret(res: Response, text: string, label: string) {
  const seen = `${[...res.headers].map(([k, v]) => `${k}: ${v}`).join("\n")}\n${text}`;
  for (const secret of [ORDER_LINK, EVIL_LINK, LINK_THROWS, SECRET])
    expect(seen, label).not.toContain(secret);
}

beforeAll(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "shop136-links-"));
  const theme = await buildArtifact(root, "theme", hostileTheme("lx"), { "_astro/app.lx.js": "1" });
  const checkout = await buildArtifact(root, "checkout", platformCheckoutWorker, {
    "_checkout/0123456789abcdef/app.js": "1",
  });
  api = fakeApi();
  gw = createGateway({
    artifactRoot: root,
    resolver: new StaticResolver({
      "fresh.localhost": site({
        tenant_id: "t-fresh",
        shop_host: "fresh.localhost",
        theme_artifact: theme.id,
        checkout_origins: [sameOrigin(FRESH)],
      }),
      "demo.localhost": site({ theme_artifact: theme.id }),
    }),
    checkoutArtifact: checkout.id,
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream: api.fn,
    renderTimeoutMs: 1500,
    log: (entry) => logged.push(entry),
    packetaWidgetUrl: "http://mocks.localhost:8280/packeta/library.js",
    pageTokenSecret: "test-page-token-secret-0123456789abcdef",
  });
});

beforeEach(() => {
  api.calls.length = 0;
  logged.length = 0;
});

afterAll(async () => {
  await gw?.dispose();
});

describe("an emailed order link becomes a grant cookie on a clean URL (C5)", () => {
  test("an admitted navigation exchanges the link and revokes the previous grant", async () => {
    const res = await get(`${FRESH}/o/${ORDER_LINK}?utm_source=mail`, {
      ...NAVIGATE,
      cookie: `__Host-Http-cap=${OLD_GRANT}; __Host-Http-cart=${CART}`,
    });
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(`/o/${ORDER_ID}`);
    expect(
      res.headers.getSetCookie(),
      "a session cookie: the server's 30-minute idle expiry is the only clock",
    ).toEqual([`__Host-Http-cap=${LINK_GRANT}; Path=/; HttpOnly; Secure; SameSite=Lax`]);
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.get("referrer-policy")).toBe("strict-origin");
    expectNoSecret(res, await res.text(), "exchange response");
    const [call] = exchanges();
    expect(exchanges()).toHaveLength(1);
    expect(call?.method).toBe("POST");
    expect(call?.headers["x-shop-origin"]).toBe(FRESH);
    expect(JSON.parse(call?.body ?? "")).toEqual({
      link: { kind: "order", token: ORDER_LINK },
      previous: OLD_GRANT,
    });
  });

  test("the email link stays reusable: each open exchanges again", async () => {
    for (let i = 0; i < 2; i++) {
      const res = await get(`${FRESH}/o/${ORDER_LINK}`, NAVIGATE);
      expect(res.headers.get("location")).toBe(`/o/${ORDER_ID}`);
    }
    expect(exchanges()).toHaveLength(2);
    expect(JSON.parse(exchanges()[0]?.body ?? "")).toEqual({
      link: { kind: "order", token: ORDER_LINK },
    });
  });

  test.each(REFUSED)(
    "a %s is refused before the exchange and consumes nothing",
    async (_, headers) => {
      const res = await get(`${FRESH}/o/${ORDER_LINK}`, {
        ...headers,
        cookie: `__Host-Http-cap=${OLD_GRANT}`,
      });
      expect(res.status).toBe(403);
      expect(await res.text()).toBe("");
      expect(res.headers.getSetCookie()).toEqual([]);
      expect(api.calls).toEqual([]);
    },
  );

  test("a browser without Fetch Metadata and a POST never reach the exchange", async () => {
    expect((await get(`${FRESH}/o/${ORDER_LINK}`)).status).toBe(400);
    const post = await get(`${FRESH}/o/${ORDER_LINK}`, NAVIGATE, { method: "POST", body: "x=1" });
    expect(post.status).toBe(405);
    expect(api.calls).toEqual([]);
  });

  test("an unknown link and an off-site resource fail closed to the reopen page", async () => {
    for (const link of ["0".repeat(64), EVIL_LINK]) {
      const res = await get(`${FRESH}/o/${link}`, {
        ...NAVIGATE,
        cookie: `__Host-Http-cap=${OLD_GRANT}`,
      });
      expect(res.status, link).toBe(303);
      expect(res.headers.get("location"), link).toBe("/o");
      expect(res.headers.getSetCookie(), link).toEqual([]);
      expectNoSecret(res, await res.text(), link);
    }
  });

  test.each([
    ["short", `/o/${ORDER_LINK.slice(1)}`],
    ["long", `/o/${ORDER_LINK}0`],
    ["upper case", `/o/${ORDER_LINK.toUpperCase()}`],
    ["encoded", `/o/%31${ORDER_LINK.slice(1)}`],
    ["nested", `/o/${ORDER_LINK}/x`],
    ["link and query token", `/o/${ORDER_LINK}?token=${SECRET}`],
    [
      "link and Stripe return",
      `/o/${ORDER_LINK}?payment_intent=pi_1&payment_intent_client_secret=${SECRET}&redirect_status=succeeded`,
    ],
    ["link and identify code", `/o/${ORDER_LINK}?code=${SECRET}`],
  ])("a malformed or doubled link (%s) fails closed without an exchange", async (_, link) => {
    const res = await get(`${FRESH}${link}`, NAVIGATE);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("/o");
    expect(res.headers.getSetCookie()).toEqual([]);
    expectNoSecret(res, await res.text(), link);
    expect(api.calls).toEqual([]);
  });

  test("an isolated shop exchanges nothing", async () => {
    await get(`${DEMO}/o/${ORDER_LINK}`, NAVIGATE);
    expect(exchanges()).toEqual([]);
  });
});

describe("every C5 parameter is stripped before a platform page renders", () => {
  describe.each(CLEAN_PAGES)("%s", (page) => {
    test.each(C5_PARAMETERS)("?%s= answers 303 to the URL without it", async (name) => {
      const res = await get(
        `${FRESH}${page}?error=stale_page&${name}=${SECRET}&${name}=${SECRET}`,
        NAVIGATE,
      );
      const stripeReturn = !["token", "t", "code"].includes(name);
      const doubledLink = LINK_POSITIONS.get(page) === name;
      expect(res.status).toBe(303);
      expect(res.headers.get("location")).toBe(
        doubledLink
          ? `${page === "/review" ? "/review" : "/withdraw/declaration"}?link=invalid`
          : `${page}?error=stale_page${stripeReturn ? "&returned=1" : ""}`,
      );
      expect(res.headers.get("cache-control")).toBe("no-store");
      expect(res.headers.getSetCookie()).toEqual([]);
      expectNoSecret(res, await res.text(), name);
      expect(api.calls).toEqual([]);
    });
  });

  test("a parameter name matches exactly and in any case", async () => {
    const res = await get(`${FRESH}/checkout?Token=${SECRET}&tokens=1`, NAVIGATE);
    expect(res.headers.get("location")).toBe("/checkout?tokens=1");
  });
});

describe("no request path reaches Location", () => {
  test.each([
    `//checkout%2f@attacker.example?token=${SECRET}`,
    `//attacker.example/%252e%252e/checkout?token=${SECRET}`,
    `//attacker.example/checkout?client_secret=${SECRET}`,
    `/checkout%2f%2fattacker.example?t=${SECRET}`,
    `/checkout%5c%5cattacker.example?t=${SECRET}`,
    `/%2e%2e/checkout?code=${SECRET}`,
    `/checkout/%2e%2e//attacker.example?token=${SECRET}`,
    `/checkout/..//attacker.example?token=${SECRET}`,
    `/\\attacker.example/checkout?token=${SECRET}`,
    `/o/x/..//attacker.example?token=${SECRET}`,
    `/checkout@attacker.example?token=${SECRET}`,
    `/CHECKOUT?token=${SECRET}`,
    `/cs/checkout?token=${SECRET}`,
  ])("%s", async (raw) => {
    const res = await gw.fetch(
      new Request(`${FRESH}${raw}`, { headers: { host: "fresh.localhost:8280", ...NAVIGATE } }),
    );
    const location = res.headers.get("location");
    if (location !== null) {
      expect(location, "a same-origin path").toMatch(/^\/(?![/\\])/);
      expect(new URL(location, FRESH).origin).toBe(FRESH);
      expect(["/checkout", "/o"], "a canonical platform route").toContain(
        new URL(location, FRESH).pathname,
      );
    }
    expectNoSecret(res, await res.text(), raw);
  });
});

describe("the clean order URL", () => {
  test.each([
    ["no grant", `/o/${ORDER_ID}`, {}],
    ["the reopen path", "/o", { cookie: `__Host-Http-cap=${LINK_GRANT}` }],
  ])("%s shows the reopen-link state", async (_, page, headers) => {
    const res = await get(`${FRESH}${page}`, { ...NAVIGATE, ...headers });
    expect(res.status).toBe(404);
    expect(res.headers.get("content-type")).toBe("text/html; charset=utf-8");
    expect(res.headers.get("cache-control")).toBe("no-store");
    const html = await res.text();
    expect(html).toContain("Otevřete prosím odkaz z e-mailu znovu");
    expect(html).toContain("open the link from your email again");
    expect(html).not.toMatch(/<script|page-token/);
    expect(api.calls).toEqual([]);
  });

  test("with a grant it renders through the grant alone", async () => {
    const res = await get(`${FRESH}/o/${ORDER_ID}`, {
      ...NAVIGATE,
      cookie: `__Host-Http-cap=${LINK_GRANT}`,
    });
    expect(res.status).toBe(200);
    const html = await res.text();
    const names = /<p id="headers">(.*?)<\/p>/.exec(html)?.[1]?.split(",") ?? [];
    expect(names, "the grant's page token only").toContain("x-platform-grant-page-token");
    expect(names).not.toContain("x-platform-page-token");
    expect(html).not.toContain(LINK_GRANT);
    const reads = api.calls.filter((c) =>
      c.url.startsWith("http://api.test/storefront/v1/checkout"),
    );
    expect(reads.length).toBeGreaterThan(0);
    for (const r of reads) {
      expect(r.headers["x-link-grant"]).toBe(LINK_GRANT);
      expect(r.headers["x-shop-origin"]).toBe(FRESH);
      expect(r.headers["x-cart-token"]).toBeUndefined();
    }
  });
});

describe("no raw link or grant reaches a log or an error", () => {
  test("an upstream failure during the exchange logs a redacted path", async () => {
    const res = await get(`${FRESH}/o/${LINK_THROWS}`, NAVIGATE);
    expect(res.status).toBeGreaterThanOrEqual(500);
    expectNoSecret(res, await res.text(), "error response");
    expect(logged.length).toBeGreaterThan(0);
    expect(JSON.stringify(logged)).not.toContain(LINK_THROWS);
    expect(JSON.stringify(logged)).toContain("/o/[redacted]");
  });

  test("a successful exchange logs neither the link nor the grant", async () => {
    await get(`${FRESH}/o/${ORDER_LINK}`, NAVIGATE);
    await get(`${FRESH}/o/${ORDER_ID}`, { ...NAVIGATE, cookie: `__Host-Http-cap=${LINK_GRANT}` });
    const text = JSON.stringify(logged);
    expect(text).not.toContain(ORDER_LINK);
    expect(text).not.toContain(LINK_GRANT);
  });
});
