import { createHmac } from "node:crypto";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import { buildArtifact, fakeApi, hostileTheme, platformCheckoutWorker, site } from "./fixtures.ts";

const SECRET = "test-page-token-secret-0123456789abcdef";
const FRESH = "http://fresh.localhost:8280";
const BARE = "http://bare.localhost:8280";
const ACCOUNT = "http://fresh.accounts.localhost:8280";
const CAP = "sameorigintoken_00000000000000000042";
const NEW_CAP = "sameorigintoken_00000000000000000077";
const STATE = "a".repeat(64);
const CODE = "c".repeat(64);
const ORDER = "0b7e2c4a-5d1f-4e3a-9c2b-7a6f5e4d3c2b";
const PLACE_API = "http://api.test/storefront/v1/checkout/place-order";
const CALLBACK_API = "http://api.test/storefront/v1/checkout/placement/callback";
const pageTokenOf = (host: string, capability: string) =>
  createHmac("sha256", SECRET).update(`pt1|${host}|${capability}`).digest("base64url");
const ROTATED_COOKIE = `__Host-Http-cart=${NEW_CAP}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=86400`;
const FAILED = "/checkout?error=placement_failed";

let api: ReturnType<typeof fakeApi>;
let gw: Gateway;
let answers: Record<string, () => Response>;

const headers = (over: Record<string, string>, base: Record<string, string>) => {
  const h = { ...base, ...over };
  for (const [name, value] of Object.entries(h)) if (value === "missing") delete h[name];
  return h;
};

const placeForm = (base = FRESH) =>
  gw.fetch(
    new Request(`${base}/checkout/place-order`, {
      method: "POST",
      body: new URLSearchParams([
        ["_pt", pageTokenOf(new URL(base).host, CAP)],
        ["version", "3"],
        ["total_minor", "12900"],
        ["accept_terms", "1"],
        ["accept_withdrawal", "1"],
        ["notes", "Zvonit dvakrát"],
      ]).toString(),
      headers: {
        host: new URL(base).host,
        origin: base,
        "sec-fetch-site": "same-origin",
        "sec-fetch-mode": "navigate",
        "sec-fetch-dest": "document",
        "content-type": "application/x-www-form-urlencoded",
        cookie: `__Host-Http-cart=${CAP}`,
      },
    }),
  );

const callback = (
  query = `state=${STATE}&code=${CODE}`,
  over: Record<string, string> = {},
  { method = "GET", route = "/checkout/placement" } = {},
) =>
  gw.fetch(
    new Request(`${FRESH}${route}?${query}`, {
      method,
      headers: headers(over, {
        host: "fresh.localhost:8280",
        "sec-fetch-site": "cross-site",
        "sec-fetch-mode": "navigate",
        "sec-fetch-dest": "document",
        cookie: `__Host-Http-cart=${CAP}`,
      }),
    }),
  );

const callsTo = (url: string) => api.calls.filter((c) => c.url === url);
const confirmed =
  (body: object, extra: Record<string, string> = {}) =>
  () =>
    Response.json(body, { status: 201, headers: extra });

function expectNoSecrets(res: Response, label: string) {
  const location = res.headers.get("location") ?? "";
  for (const secret of [CODE, CAP, NEW_CAP])
    expect(location, `${label}: no secret in Location`).not.toContain(secret);
}

beforeAll(async () => {
  api = fakeApi();
  const artifactRoot = await mkdtemp(path.join(tmpdir(), "shop143-placement-"));
  const checkout = await buildArtifact(artifactRoot, "checkout", platformCheckoutWorker, {
    "_checkout/0123456789abcdef/app.js": "",
  });
  const theme = await buildArtifact(artifactRoot, "theme", hostileTheme("pl"), {
    "_astro/app.pl.js": "1",
  });
  const sameOrigin = (host: string, origin: string, over = {}) =>
    site({
      tenant_id: `t-${host}`,
      shop_host: `${host}.localhost`,
      checkout_origins: [
        { origin, mode: "same_origin", mode_generation: 1, checkout_origin: origin },
      ],
      ...over,
    });
  gw = createGateway({
    artifactRoot,
    checkoutArtifact: checkout.id,
    resolver: new StaticResolver({
      "fresh.localhost": sameOrigin("fresh", FRESH, {
        account_host: "fresh.accounts.localhost",
        theme_artifact: theme.id,
      }),
      "bare.localhost": sameOrigin("bare", BARE),
    }),
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http" as const,
    purgeToken: "purge-token-0123456789abcdef",
    pageTokenSecret: SECRET,
    upstream: async (req: Request) => {
      const res = await api.fn(req);
      return answers[req.url]?.() ?? res;
    },
  });
});

beforeEach(() => {
  api.calls.length = 0;
  answers = {
    [PLACE_API]: () => Response.json({ state: STATE }, { status: 202 }),
    [CALLBACK_API]: confirmed({ order_id: ORDER, number: "100001", as_guest: false }),
  };
});

afterAll(async () => {
  await gw?.dispose();
});

describe("place-order of a linked checkout (A2 step 4)", () => {
  test("sends the browser to the account origin with the placement state", async () => {
    const res = await placeForm();
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(
      `${ACCOUNT}/placement?state=${STATE}&origin=${encodeURIComponent(FRESH)}`,
    );
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.getSetCookie(), "no capability yet").toEqual([]);
    expect(callsTo(PLACE_API)).toHaveLength(1);
    expect(callsTo(CALLBACK_API)).toEqual([]);
  });

  test("a malformed state from the API is not used", async () => {
    answers[PLACE_API] = () => Response.json({ state: "short" }, { status: 202 });
    const res = await placeForm();
    expect(res.headers.get("location")).toBe(FAILED);
  });

  test("a shop without an account origin places nothing", async () => {
    const res = await placeForm(BARE);
    expect(res.headers.get("location")).toBe(FAILED);
  });
});

describe("placement callback (A2 step 4, C5)", () => {
  test("the expected account lands on the clean order page", async () => {
    const res = await callback();
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(`/o/${ORDER}`);
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.getSetCookie()).toEqual([]);
    expect(await res.text()).toBe("");
    const calls = callsTo(CALLBACK_API);
    expect(calls).toHaveLength(1);
    expect(api.calls, "no other upstream call").toHaveLength(1);
    expect(calls[0]?.method).toBe("POST");
    expect(calls[0]?.headers["x-cart-token"]).toBe(CAP);
    expect(calls[0]?.headers["x-shop-origin"]).toBe(FRESH);
    expect(JSON.parse(calls[0]?.body ?? "")).toEqual({ state: STATE, code: CODE });
  });

  test("a guest placement shows the receipt before any payment step", async () => {
    answers[CALLBACK_API] = confirmed({ order_id: ORDER, number: "100001", as_guest: true });
    const res = await callback();
    expect(res.headers.get("location")).toBe("/checkout");
  });

  test("a placement that wrote a note replaces the capability", async () => {
    answers[CALLBACK_API] = confirmed(
      { order_id: ORDER, number: "100001", as_guest: true },
      { "x-cart-token": NEW_CAP },
    );
    const res = await callback();
    expect(res.headers.getSetCookie()).toEqual([ROTATED_COOKIE]);
  });

  test("an order id that is not a UUID falls back to the receipt", async () => {
    answers[CALLBACK_API] = confirmed({ order_id: "../x", number: "1", as_guest: false });
    const res = await callback();
    expect(res.headers.get("location")).toBe("/checkout");
  });

  test.each([400, 403, 404, 409, 422, 500, 502])(
    "a refused callback (%i) goes back to review with no order and no cookie",
    async (status) => {
      answers[CALLBACK_API] = () => new Response(null, { status });
      const res = await callback();
      expect(res.status).toBe(303);
      expect(res.headers.get("location")).toBe(FAILED);
      expect(res.headers.getSetCookie()).toEqual([]);
    },
  );

  test("a network failure is never a guest placement", async () => {
    answers[CALLBACK_API] = () => {
      throw new Error("connection reset");
    };
    const res = await callback();
    expect(res.status).toBeGreaterThanOrEqual(500);
    expect(res.headers.get("location")).toBeNull();
  });

  test.each([
    ["no state", `code=${CODE}`],
    ["no code", `state=${STATE}`],
    ["an empty code", `state=${STATE}&code=`],
    ["two codes", `state=${STATE}&code=${CODE}&code=${"d".repeat(64)}`],
    ["an extra parameter", `state=${STATE}&code=${CODE}&next=//evil.example`],
    ["a note", `state=${STATE}&code=${CODE}&notes=changed`],
    ["an upper-case name", `STATE=${STATE}&code=${CODE}`],
  ])("%s fails closed without an exchange", async (label, query) => {
    const res = await callback(query);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(FAILED);
    expectNoSecrets(res, label);
    expect(api.calls).toEqual([]);
  });

  test("no capability fails closed without an exchange", async () => {
    const res = await callback(undefined, { cookie: "" });
    expect(res.headers.get("location")).toBe(FAILED);
    expect(api.calls).toEqual([]);
  });

  test.each([
    ["a fetch", { "sec-fetch-mode": "cors", "sec-fetch-dest": "empty" }],
    ["an iframe", { "sec-fetch-dest": "iframe" }],
    ["a prefetch", { "sec-purpose": "prefetch" }],
  ])("%s consumes nothing (C1)", async (_, over) => {
    const res = await callback(undefined, over);
    expect(res.status).toBe(403);
    expect(api.calls).toEqual([]);
  });

  test.each(["POST", "HEAD"])("%s is not allowed", async (method) => {
    const res = await callback(undefined, {}, { method });
    expect(res.status).toBe(405);
    expect(api.calls).toEqual([]);
  });

  test.each([
    "/checkout/placement/",
    "/Checkout/placement",
    "/checkout//placement",
    "/cs/checkout/placement",
    "/checkout/%70lacement",
  ])("%s is no callback and never echoes the code", async (route) => {
    const res = await callback(undefined, {}, { route });
    expect(callsTo(CALLBACK_API)).toEqual([]);
    expect(res.headers.get("location") ?? "").not.toContain(CODE);
  });
});
