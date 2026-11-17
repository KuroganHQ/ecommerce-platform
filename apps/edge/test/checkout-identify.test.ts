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
const START_API = "http://api.test/storefront/v1/checkout/identify";
const CALLBACK_API = "http://api.test/storefront/v1/checkout/identify/callback";
const pageTokenOf = (host: string, capability: string) =>
  createHmac("sha256", SECRET).update(`pt1|${host}|${capability}`).digest("base64url");
const TOKEN = pageTokenOf("fresh.localhost:8280", CAP);
const IDT_COOKIE = `__Host-Http-idt=${STATE}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=300`;
const CLEAR_IDT = "__Host-Http-idt=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0";
const ROTATED_COOKIE = `__Host-Http-cart=${NEW_CAP}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=86400`;
const FAILED = "/checkout?error=identify_failed";

let api: ReturnType<typeof fakeApi>;
let gw: Gateway;
let answers: Record<string, () => Response>;

const headers = (over: Record<string, string>, base: Record<string, string>) => {
  const h = { ...base, ...over };
  for (const [name, value] of Object.entries(h)) if (value === "missing") delete h[name];
  return h;
};

const startForm = (over: Record<string, string> = {}, body = `_pt=${TOKEN}`, base = FRESH) =>
  gw.fetch(
    new Request(`${base}/checkout/identify/start`, {
      method: "POST",
      body,
      headers: headers(over, {
        host: new URL(base).host,
        origin: base,
        "sec-fetch-site": "same-origin",
        "sec-fetch-mode": "navigate",
        "sec-fetch-dest": "document",
        "content-type": "application/x-www-form-urlencoded",
        cookie: `__Host-Http-cart=${CAP}`,
      }),
    }),
  );

const callback = (
  query = `state=${STATE}&code=${CODE}`,
  over: Record<string, string> = {},
  { method = "GET", route = "/checkout/identify" } = {},
) =>
  gw.fetch(
    new Request(`${FRESH}${route}?${query}`, {
      method,
      headers: headers(over, {
        host: "fresh.localhost:8280",
        "sec-fetch-site": "cross-site",
        "sec-fetch-mode": "navigate",
        "sec-fetch-dest": "document",
        cookie: `__Host-Http-cart=${CAP}; __Host-Http-idt=${STATE}`,
      }),
    }),
  );

const callsTo = (url: string) => api.calls.filter((c) => c.url === url);

function expectNoSecrets(res: Response, label: string) {
  const location = res.headers.get("location") ?? "";
  for (const secret of [STATE, CODE, CAP, NEW_CAP, TOKEN])
    expect(location, `${label}: no secret in Location`).not.toContain(secret);
}

function expectNoIdtWritten(res: Response, label: string) {
  for (const c of res.headers.getSetCookie())
    expect(c.startsWith("__Host-Http-idt=") && c !== CLEAR_IDT, `${label}: ${c}`).toBe(false);
}

beforeAll(async () => {
  api = fakeApi();
  const artifactRoot = await mkdtemp(path.join(tmpdir(), "shop142-identify-"));
  const checkout = await buildArtifact(artifactRoot, "checkout", platformCheckoutWorker, {
    "_checkout/0123456789abcdef/app.js": "",
  });
  const theme = await buildArtifact(artifactRoot, "theme", hostileTheme("id"), {
    "_astro/app.id.js": "1",
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
      "iso.localhost": site({
        tenant_id: "t-iso",
        shop_host: "iso.localhost",
        account_host: "iso.accounts.localhost",
        theme_artifact: theme.id,
        checkout_artifact: checkout.id,
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
    [START_API]: () => Response.json({ state: STATE }, { status: 201 }),
    [CALLBACK_API]: () => new Response(null, { status: 204, headers: { "x-cart-token": NEW_CAP } }),
  };
});

afterAll(async () => {
  await gw?.dispose();
});

describe("identify start (A2 step 1)", () => {
  test("mints the state, sets it as the idt cookie and sends the browser to the account origin", async () => {
    const res = await startForm();
    expect(res.status).toBe(303);
    const location = new URL(res.headers.get("location") ?? "");
    expect(location.origin).toBe(ACCOUNT);
    expect(location.pathname).toBe("/identify");
    expect([...location.searchParams]).toEqual([
      ["state", STATE],
      ["origin", FRESH],
    ]);
    expect(res.headers.getSetCookie()).toEqual([IDT_COOKIE]);
    expect(res.headers.get("cache-control")).toBe("no-store");
    const [call] = callsTo(START_API);
    expect(call?.method).toBe("POST");
    expect(call?.headers["x-cart-token"]).toBe(CAP);
    expect(call?.headers["x-shop-origin"]).toBe(FRESH);
    expect(JSON.stringify(api.calls), "the API never receives the page token").not.toContain(TOKEN);
  });

  test.each([
    ["no page token", {}, ""],
    ["a wrong page token", {}, `_pt=${pageTokenOf("fresh.localhost:8280", NEW_CAP)}`],
    ["no capability", { cookie: "missing" }, `_pt=${TOKEN}`],
  ])("%s gets the stale page and no state", async (_, over, body) => {
    const res = await startForm(over, body);
    expect(res.status).toBe(403);
    expect(res.headers.getSetCookie()).toEqual([]);
    expect(api.calls).toEqual([]);
  });

  test.each([
    ["a cross-site Origin", { origin: "http://evil.localhost:8280" }],
    ["a fetch", { "sec-fetch-mode": "cors" }],
    ["a same-site request", { "sec-fetch-site": "same-site" }],
    ["a prefetch", { "sec-purpose": "prefetch" }],
  ])("refuses %s", async (_, over) => {
    const res = await startForm(over);
    expect(res.status).toBe(403);
    expect(res.headers.getSetCookie()).toEqual([]);
    expect(api.calls).toEqual([]);
  });

  test("GET is not allowed", async () => {
    const res = await gw.fetch(
      new Request(`${FRESH}/checkout/identify/start`, {
        headers: {
          host: "fresh.localhost:8280",
          "sec-fetch-mode": "navigate",
          "sec-fetch-dest": "document",
        },
      }),
    );
    expect(res.status).toBe(405);
    expect(api.calls).toEqual([]);
  });

  test.each([
    [404, "/checkout"],
    [409, "/checkout"],
    [500, FAILED],
  ])("an API %i sets no cookie and goes to %s", async (status, location) => {
    answers[START_API] = () => new Response(null, { status });
    const res = await startForm();
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(location);
    expect(res.headers.getSetCookie()).toEqual([]);
  });

  test("a malformed state from the API is not used", async () => {
    answers[START_API] = () => Response.json({ state: "x y" }, { status: 201 });
    const res = await startForm();
    expect(res.headers.get("location")).toBe(FAILED);
    expect(res.headers.getSetCookie()).toEqual([]);
  });

  test("a shop without an account origin offers no identify", async () => {
    const bareToken = pageTokenOf("bare.localhost:8280", CAP);
    const res = await startForm({}, `_pt=${bareToken}`, BARE);
    expect(res.headers.get("location")).toBe(FAILED);
    expect(res.headers.getSetCookie()).toEqual([]);
    expect(api.calls).toEqual([]);
  });
});

describe("identify callback (A2 steps 2-3, C5)", () => {
  test("exchanges state, idt cookie and code once, rotates the capability and lands on a clean URL", async () => {
    const res = await callback();
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("/checkout");
    expect(res.headers.getSetCookie()).toEqual([ROTATED_COOKIE, CLEAR_IDT]);
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.get("referrer-policy")).toBe("strict-origin");
    expect(await res.text()).toBe("");
    const calls = callsTo(CALLBACK_API);
    expect(calls).toHaveLength(1);
    expect(api.calls, "no other upstream call").toHaveLength(1);
    expect(calls[0]?.method).toBe("POST");
    expect(calls[0]?.headers["x-cart-token"]).toBe(CAP);
    expect(calls[0]?.headers["x-shop-origin"]).toBe(FRESH);
    expect(JSON.parse(calls[0]?.body ?? "")).toEqual({ state: STATE, idt: STATE, code: CODE });
  });

  test("the idt cookie, not the URL, is what the state must match", async () => {
    await callback(`state=${STATE}&code=${CODE}`, {
      cookie: `__Host-Http-cart=${CAP}; __Host-Http-idt=${"b".repeat(64)}`,
    });
    expect(JSON.parse(callsTo(CALLBACK_API)[0]?.body ?? "")).toEqual({
      state: STATE,
      idt: "b".repeat(64),
      code: CODE,
    });
  });

  test.each([404, 409, 403, 500])("a refused callback (%i) changes no cookie", async (status) => {
    answers[CALLBACK_API] = () => new Response(null, { status });
    const res = await callback();
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(FAILED);
    expect(res.headers.getSetCookie(), "the expected idt stays").toEqual([]);
  });

  test.each([
    ["no state", `code=${CODE}`],
    ["no code", `state=${STATE}`],
    ["an empty code", `state=${STATE}&code=`],
    ["two codes", `state=${STATE}&code=${CODE}&code=${"d".repeat(64)}`],
    ["two states", `state=${STATE}&state=${"b".repeat(64)}&code=${CODE}`],
    ["an extra parameter", `state=${STATE}&code=${CODE}&next=//evil.example`],
    ["a token next to the code", `state=${STATE}&code=${CODE}&token=${"e".repeat(64)}`],
    ["an upper-case name", `STATE=${STATE}&code=${CODE}`],
  ])("%s fails closed without an exchange", async (label, query) => {
    const res = await callback(query);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(FAILED);
    expect(res.headers.getSetCookie()).toEqual([]);
    expectNoSecrets(res, label);
    expect(api.calls).toEqual([]);
  });

  test.each([
    ["no capability", `__Host-Http-idt=${STATE}`],
    ["no idt cookie", `__Host-Http-cart=${CAP}`],
    ["no cookies", ""],
  ])("%s fails closed without an exchange", async (label, cookie) => {
    const res = await callback(undefined, { cookie });
    expect(res.headers.get("location")).toBe(FAILED);
    expect(res.headers.getSetCookie()).toEqual([]);
    expectNoSecrets(res, label);
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
    "/checkout/identify/",
    "/Checkout/identify",
    "/checkout//identify",
    "/checkout/identify/start",
    "/cs/checkout/identify",
    "/checkout/%69dentify",
  ])("%s is no callback and never echoes the code", async (route) => {
    const res = await callback(undefined, {}, { route });
    expect(callsTo(CALLBACK_API)).toEqual([]);
    expect(res.headers.get("location") ?? "", "the code never reaches a Location").not.toContain(
      CODE,
    );
    expectNoIdtWritten(res, route);
  });

  test("a link exchange never writes the idt cookie", async () => {
    const res = await callback("", {}, { route: `/o/${"f".repeat(64)}` });
    expectNoIdtWritten(res, "order link");
  });
});

describe("the checkout page may send its sign-in form to the account origin only", () => {
  const page = (base: string) =>
    gw.fetch(
      new Request(`${base}/checkout`, {
        headers: {
          host: new URL(base).host,
          "sec-fetch-mode": "navigate",
          "sec-fetch-dest": "document",
          cookie: `__Host-Http-cart=${CAP}`,
        },
      }),
    );
  const formAction = (res: Response) =>
    (res.headers.get("content-security-policy") ?? "")
      .split(";")
      .map((d) => d.trim())
      .find((d) => d.startsWith("form-action"));

  test("with an account origin", async () => {
    expect(formAction(await page(FRESH))).toBe(`form-action 'self' ${ACCOUNT}`);
  });

  test("without one", async () => {
    expect(formAction(await page(BARE))).toBe("form-action 'self'");
  });

  const csp = async (url: string, headers: Record<string, string> = {}) => {
    const res = await gw.fetch(
      new Request(url, {
        headers: {
          host: new URL(url).host,
          "sec-fetch-mode": "navigate",
          "sec-fetch-dest": "document",
          ...headers,
        },
      }),
    );
    return res.headers.get("content-security-policy") ?? "";
  };

  test.each([
    ["the same-origin shop's theme page", `${FRESH}/`, {}],
    [
      "the same-origin order page",
      `${FRESH}/o/0190a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b`,
      { cookie: `__Host-Http-cap=${CAP}` },
    ],
    ["an isolated shop's theme page", "http://iso.localhost:8280/", {}],
    ["an isolated checkout page", "http://checkout.iso.localhost:8280/", {}],
  ])("%s never allows the account origin", async (_, url, headers) => {
    const policy = await csp(url, headers);
    expect(policy, "a CSP is set").toContain("form-action");
    expect(policy).not.toContain("accounts.localhost");
  });
});
