import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, fakeApi, site } from "./fixtures.ts";

const FRESH = "http://fresh.localhost:8280";
const LEGACY = "http://checkout.fresh.localhost:8280";
const ACCOUNT = "http://fresh.accounts.localhost:8280";
const ISOLATED_LEGACY = "http://checkout.iso.localhost:8280";
const TOKEN = "7a".repeat(32);
const ORDER_ID = "0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";

const worker = `
export default {
  async fetch() {
    return new Response("<!doctype html><p>checkout", { headers: { "content-type": "text/html" } });
  },
};
`;

let gw: Gateway;
let calls: string[];

const send = (url: string, init: RequestInit & { headers?: Record<string, string> } = {}) =>
  gw.fetch(
    new Request(url, { ...init, headers: { host: new URL(url).host, ...(init.headers ?? {}) } }),
  );

beforeAll(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "shop141-"));
  const checkout = await buildArtifact(root, "checkout", worker, {
    "_checkout/0123456789abcdef/app.js": "1",
  });
  const api = fakeApi();
  gw = createGateway({
    artifactRoot: root,
    resolver: new StaticResolver({
      "fresh.localhost": site({
        tenant_id: "t-fresh",
        shop_host: "fresh.localhost",
        account_host: "fresh.accounts.localhost",
        checkout_origins: [
          { origin: FRESH, mode: "same_origin", mode_generation: 1, checkout_origin: FRESH },
        ],
      }),
      "iso.localhost": site({ tenant_id: "t-iso", shop_host: "iso.localhost" }),
    }),
    checkoutArtifact: checkout.id,
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream: async (req) => {
      calls.push(`${req.method} ${new URL(req.url).pathname.replace("/storefront/v1", "")}`);
      if (new URL(req.url).pathname.endsWith("/unsubscribe"))
        return Response.json({ status: "unsubscribed" });
      return api.fn(req);
    },
    pageTokenSecret: "test-page-token-secret-0123456789abcdef",
  });
});

beforeEach(() => {
  calls = [];
});

afterAll(async () => {
  await gw?.dispose();
});

const OLD_PAGES: [string, string][] = [
  ["/", "/checkout"],
  [`/o/${TOKEN}`, `/o/${TOKEN}`],
  ["/withdraw", "/withdraw"],
  ["/review", "/review"],
  ["/restore-cart", "/restore-cart"],
  ["/consent", "/consent"],
  ["/newsletter", "/newsletter"],
  ["/newsletter/confirm", "/newsletter/confirm"],
  ["/watch/confirm", "/watch/confirm"],
  ["/watch/unsubscribe", "/watch/unsubscribe"],
  ["/watch/unsubscribed", "/watch/unsubscribed"],
  ["/flows/unsubscribe", "/flows/unsubscribe"],
  ["/flows/unsubscribed", "/flows/unsubscribed"],
  ["/_p/newsletter/click", "/newsletter/click"],
  ["/_p/newsletter/unsubscribe", "/newsletter"],
  ["/_p/flows/unsubscribe", "/flows/unsubscribe"],
];

function expectNotFound(res: Response) {
  expect(res.status).toBe(404);
  expect(res.headers.get("location")).toBeNull();
  expect(res.headers.get("set-cookie")).toBeNull();
  expect(res.headers.get("cache-control")).toBe("no-store");
}

describe("checkout.<shop> of a same-origin shop answers only the legacy map", () => {
  test.each(OLD_PAGES)("%s → 308 %s on the shop, query kept, no-store", async (from, to) => {
    for (const method of ["GET", "HEAD"]) {
      const res = await send(`${LEGACY}${from}?token=${TOKEN}&utm_source=mail`, { method });
      expect(res.status).toBe(308);
      expect(res.headers.get("location")).toBe(`${FRESH}${to}?token=${TOKEN}&utm_source=mail`);
      expect(res.headers.get("cache-control")).toBe("no-store");
      expect(res.headers.get("referrer-policy")).toBe("no-referrer");
      expect(res.headers.get("set-cookie")).toBeNull();
    }
    expect(calls).toEqual([]);
  });

  test("a page without a query keeps none", async () => {
    const res = await send(`${LEGACY}/withdraw`);
    expect(res.headers.get("location")).toBe(`${FRESH}/withdraw`);
  });

  test.each([
    ["/account", `${ACCOUNT}/account`],
    ["/account/addresses", `${ACCOUNT}/account/addresses`],
    [`/account/orders/${ORDER_ID}`, `${ACCOUNT}/account/orders/${ORDER_ID}`],
  ])("%s goes to the account origin only", async (from, to) => {
    const res = await send(`${LEGACY}${from}?x=1`);
    expect(res.status).toBe(308);
    expect(res.headers.get("location")).toBe(`${to}?x=1`);
    expect(res.headers.get("cache-control")).toBe("no-store");
  });

  test.each([
    "//attacker.example",
    "//attacker.example/withdraw",
    "/%2Fattacker.example",
    "/%5Cattacker.example",
    "/withdraw%2F..%2Fo",
    `/o%2F${TOKEN}`,
    "/o/%37a",
    "/withdraw/",
    "/WITHDRAW",
    "/withdraw/declaration",
    `/o/${ORDER_ID}`,
    `/o/${TOKEN}/x`,
    "/checkout",
    "/cs/withdraw",
    "/account/unknown",
    "/_p/checkout/start",
    "/_p/withdraw",
    "/nonsense",
  ])("%s is not in the map: 404, no Location", async (p) => {
    expectNotFound(await send(`${LEGACY}${p}?token=${TOKEN}`));
    expect(calls).toEqual([]);
  });

  test("an old start handoff is refused, never redeemed", async () => {
    const res = await send(`${LEGACY}/start?h=${TOKEN}`, {
      headers: { cookie: `__Secure-hf-${TOKEN.slice(0, 16)}=${TOKEN}` },
    });
    expectNotFound(res);
    expect(calls).toEqual([]);
  });

  test("a page POST is not replayed onto the shop", async () => {
    expectNotFound(await send(`${LEGACY}/review`, { method: "POST", body: "rating=5" }));
    expect(calls).toEqual([]);
  });

  test.each([
    [`/_p/newsletter/unsubscribe?t=${TOKEN}`, "POST /newsletter/unsubscribe"],
    [`/_p/flows/unsubscribe?token=${TOKEN}`, "POST /flows/unsubscribe"],
  ])("old one-click POST %s still unsubscribes directly (default 12)", async (p, call) => {
    const res = await send(`${LEGACY}${p}`, {
      method: "POST",
      headers: { "content-type": "application/x-www-form-urlencoded" },
      body: "List-Unsubscribe=One-Click",
    });
    expect(res.status).toBe(200);
    expect(calls).toEqual([call]);
  });

  test("the target comes from the resolved shop, never from request headers", async () => {
    const res = await send(`${LEGACY}/withdraw`, {
      headers: { "x-forwarded-host": "attacker.example", forwarded: "host=attacker.example" },
    });
    expect(res.headers.get("location")).toBe(`${FRESH}/withdraw`);
    const forged = await gw.fetch(
      new Request(`${LEGACY}/withdraw`, { headers: { host: "checkout.attacker.example" } }),
    );
    expect(forged.status).toBe(404);
    expect(forged.headers.get("location")).toBeNull();
  });
});

test("an isolated shop's checkout.<shop> keeps serving its pages", async () => {
  const res = await send(`${ISOLATED_LEGACY}/withdraw`);
  expect(res.status).not.toBe(308);
  expect(res.headers.get("location")).toBeNull();
});
