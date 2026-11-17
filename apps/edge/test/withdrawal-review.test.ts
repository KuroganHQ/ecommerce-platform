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
const WITHDRAWAL_LINK = "3a".repeat(32);
const REVIEW_LINK = "3b".repeat(32);
const ANONYMOUS = "4a".repeat(32);
const WITHDRAWAL = "4b".repeat(32);
const REVIEW = "4c".repeat(32);
const OLD = "4d".repeat(32);
const CART = "sameorigintoken_00000000000000000042";
const tokenOf = (capability: string) =>
  createHmac("sha256", SECRET).update(`pt1|fresh.localhost:8280|${capability}`).digest("base64url");
const NAVIGATE = {
  "sec-fetch-site": "cross-site",
  "sec-fetch-mode": "navigate",
  "sec-fetch-dest": "document",
};
const FETCH = {
  origin: FRESH,
  "sec-fetch-site": "same-origin",
  "sec-fetch-mode": "cors",
  "sec-fetch-dest": "empty",
  "content-type": "application/json",
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
  ["fetch", { ...FETCH }],
  ["iframe", { ...NAVIGATE, "sec-fetch-dest": "iframe" }],
];

const grantPageWorker = `
export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const ctx = request.headers.get("x-platform-ctx");
    const read = url.pathname === "/review" ? "/link-grants/review" : "/link-grants/withdrawal";
    const res = await env.CHECKOUT.fetch("https://checkout" + read, { headers: { "x-platform-ctx": ctx } });
    const token = request.headers.get("x-platform-grant-page-token");
    const meta = token ? '<meta name="page-token" content="' + token + '">' : "";
    return new Response("<!doctype html><html><head>" + meta + "</head><body><pre>" + res.status + " " + (await res.text()) + "</pre></body></html>", {
      headers: { "content-type": "text/html" },
    });
  },
};
`;

type Call = { method: string; url: string; headers: Record<string, string>; body: string };
let calls: Call[];
let gw: Gateway;
let reviewAnswer: [number, string];

function api() {
  const base = fakeApi();
  return async (req: Request): Promise<Response> => {
    const url = new URL(req.url);
    const body = await req.clone().text();
    calls.push({
      method: req.method,
      url: req.url,
      headers: Object.fromEntries(req.headers),
      body,
    });
    const p = url.pathname.replace("/storefront/v1", "");
    const grant = req.headers.get("x-link-grant");
    if (p === "/link-grants" && req.method === "POST") {
      const link = (JSON.parse(body) as { link: { kind: string; token?: string } }).link;
      const minted =
        link.kind === "withdrawal_request"
          ? ANONYMOUS
          : link.kind === "withdrawal" && link.token === WITHDRAWAL_LINK
            ? WITHDRAWAL
            : link.kind === "review" && link.token === REVIEW_LINK
              ? REVIEW
              : null;
      if (!minted) return Response.json({ code: "not_found" }, { status: 404 });
      return Response.json(
        { resource: { kind: link.kind } },
        { status: 201, headers: { "x-link-grant": minted } },
      );
    }
    if (p === "/link-grants/withdrawal" && req.method === "GET")
      return grant === WITHDRAWAL
        ? Response.json({ order_number: "100001", grant_seen: grant })
        : Response.json({ code: "not_found" }, { status: 404 });
    if (p === "/link-grants/review" && req.method === "GET")
      return grant === REVIEW
        ? Response.json({ product_name: "Tričko", grant_seen: grant })
        : Response.json({ code: "not_found" }, { status: 404 });
    if (p === "/link-grants/review" && req.method === "POST")
      return Response.json({ code: reviewAnswer[1] }, { status: reviewAnswer[0] });
    if (p === "/link-grants/withdrawal-request") return new Response(null, { status: 202 });
    if (p === "/link-grants/withdrawal" && req.method === "POST")
      return Response.json({ declaration: "…" }, { status: 201 });
    return base.fn(req);
  };
}

const send = (
  url: string,
  headers: Record<string, string> = {},
  init: { method?: string; body?: string } = {},
) => gw.fetch(new Request(url, { ...init, headers: { host: new URL(url).host, ...headers } }));

const apiCalls = (suffix: string) =>
  calls.filter((c) => new URL(c.url).pathname === `/storefront/v1${suffix}`);

function expectPlatformHeaders(res: Response, label: string) {
  expect(res.headers.get("cache-control"), label).toBe("no-store");
  expect(res.headers.get("referrer-policy"), label).toBe("strict-origin");
}

function expectNoSecret(res: Response, label: string) {
  const seen = `${res.headers.get("location") ?? ""}`;
  for (const secret of [WITHDRAWAL_LINK, REVIEW_LINK, ANONYMOUS, WITHDRAWAL, REVIEW])
    expect(seen, label).not.toContain(secret);
}

beforeAll(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "shop138-"));
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
  reviewAnswer = [201, "pending"];
});

afterAll(async () => {
  await gw?.dispose();
});

describe("emailed withdrawal and review links exchange to grants on clean URLs (C5)", () => {
  test.each([
    [
      `/withdraw?t=${WITHDRAWAL_LINK}`,
      "withdrawal",
      WITHDRAWAL_LINK,
      "/withdraw/declaration",
      WITHDRAWAL,
    ],
    [`/review?token=${REVIEW_LINK}`, "review", REVIEW_LINK, "/review", REVIEW],
  ])(
    "%s answers 303 to its clean page with the grant cookie",
    async (path, kind, token, clean, grant) => {
      const res = await send(`${FRESH}${path}`, { ...NAVIGATE, cookie: `__Host-Http-cap=${OLD}` });
      expect(res.status).toBe(303);
      expect(res.headers.get("location")).toBe(clean);
      expect(res.headers.get("set-cookie")).toBe(
        `__Host-Http-cap=${grant}; Path=/; HttpOnly; Secure; SameSite=Lax`,
      );
      expectPlatformHeaders(res, path);
      expect(apiCalls("/link-grants").map((c) => JSON.parse(c.body))).toEqual([
        { link: { kind, token }, previous: OLD },
      ]);
    },
  );

  test.each([
    ["/withdraw?t=", "/withdraw/declaration?link=invalid", WITHDRAWAL_LINK, "t"],
    ["/review?token=", "/review?link=invalid", REVIEW_LINK, "token"],
  ])("%s fails closed to %s and keeps the cookie", async (prefix, failed, good, name) => {
    const cases = [
      `${prefix}${"0".repeat(64)}`,
      `${prefix}xyz`,
      `${prefix}${good.toUpperCase()}`,
      `${prefix}${good}&${name}=${good}`,
      `${prefix}${good}&code=${good}`,
    ];
    for (const path of cases) {
      calls = [];
      const res = await send(`${FRESH}${path}`, NAVIGATE);
      expect(res.status, path).toBe(303);
      expect(res.headers.get("location"), path).toBe(failed);
      expect(res.headers.get("set-cookie"), path).toBeNull();
      expect(apiCalls("/link-grants").length, path).toBe(path.endsWith("0".repeat(64)) ? 1 : 0);
    }
  });

  test.each(REFUSED)("a %s request exchanges nothing", async (_, headers) => {
    for (const path of [
      `/withdraw?t=${WITHDRAWAL_LINK}`,
      `/review?token=${REVIEW_LINK}`,
      "/withdraw",
    ]) {
      const res = await send(`${FRESH}${path}`, headers);
      expect(res.status, path).toBe(403);
      expect(res.headers.get("set-cookie"), path).toBeNull();
    }
    expect(calls).toEqual([]);
  });
});

describe("withdrawal pages render from the grant cookie (default 8)", () => {
  test("/withdraw mints an anonymous request grant and revokes the previous one", async () => {
    const res = await send(`${FRESH}/withdraw`, { ...NAVIGATE, cookie: `__Host-Http-cap=${OLD}` });
    expect(res.status).toBe(200);
    expectPlatformHeaders(res, "/withdraw");
    expect(res.headers.get("set-cookie")).toBe(
      `__Host-Http-cap=${ANONYMOUS}; Path=/; HttpOnly; Secure; SameSite=Lax`,
    );
    expect(apiCalls("/link-grants").map((c) => JSON.parse(c.body))).toEqual([
      { link: { kind: "withdrawal_request" }, previous: OLD },
    ]);
    expect(await res.text()).toContain(`<meta name="page-token" content="${tokenOf(ANONYMOUS)}">`);
  });

  test("/withdraw/declaration reads the form by the grant and embeds its page token", async () => {
    const res = await send(`${FRESH}/withdraw/declaration`, {
      ...NAVIGATE,
      cookie: `__Host-Http-cap=${WITHDRAWAL}`,
    });
    expect(res.status).toBe(200);
    expectPlatformHeaders(res, "/withdraw/declaration");
    const html = await res.text();
    expect(html).toContain(`<meta name="page-token" content="${tokenOf(WITHDRAWAL)}">`);
    expect(html).toContain(`200 {"order_number":"100001","grant_seen":"${WITHDRAWAL}"}`);
    expect(html).not.toContain(WITHDRAWAL_LINK);
    expect(res.headers.get("set-cookie")).toBeNull();
  });

  test("/review reads the invitation by the grant", async () => {
    const res = await send(`${FRESH}/review`, { ...NAVIGATE, cookie: `__Host-Http-cap=${REVIEW}` });
    expect(res.status).toBe(200);
    const html = await res.text();
    expect(html).toContain(`<meta name="page-token" content="${tokenOf(REVIEW)}">`);
    expect(html).toContain(`"grant_seen":"${REVIEW}"`);
  });
});

describe("withdrawal calls carry the page token, never a bearer (C4)", () => {
  test.each([
    ["/_p/withdraw", ANONYMOUS, "/link-grants/withdrawal-request", 202],
    ["/_p/withdraw/declaration", WITHDRAWAL, "/link-grants/withdrawal", 201],
  ])("%s forwards only the grant its page token matches", async (path, grant, target, status) => {
    const res = await send(
      `${FRESH}${path}`,
      { ...FETCH, cookie: `__Host-Http-cap=${grant}`, "x-page-token": tokenOf(grant) },
      { method: "POST", body: "{}" },
    );
    expect(res.status).toBe(status);
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.get("referrer-policy")).toBe("no-referrer");
    const forwarded = apiCalls(target);
    expect(forwarded.length).toBe(1);
    expect(forwarded[0]?.headers["x-link-grant"]).toBe(grant);
    expect(forwarded[0]?.headers["x-cart-token"]).toBeUndefined();
  });

  test.each([
    ["no grant cookie", `__Host-Http-cart=${CART}`, tokenOf(CART)],
    ["no page token", `__Host-Http-cap=${WITHDRAWAL}`, undefined],
    [
      "another capability's token",
      `__Host-Http-cap=${WITHDRAWAL}; __Host-Http-cart=${CART}`,
      tokenOf(CART),
    ],
    ["a stale token", `__Host-Http-cap=${WITHDRAWAL}`, tokenOf(OLD)],
  ])("%s is refused before the API", async (_, cookie, token) => {
    const res = await send(
      `${FRESH}/_p/withdraw/declaration`,
      { ...FETCH, cookie, ...(token ? { "x-page-token": token } : {}) },
      { method: "POST", body: "{}" },
    );
    expect(res.status).toBe(403);
    expect(calls).toEqual([]);
  });

  test("bearer paths, reads and cross-origin calls reach no API", async () => {
    const cookie = `__Host-Http-cap=${WITHDRAWAL}`;
    const token = tokenOf(WITHDRAWAL);
    const cases: [string, Record<string, string>, string, number][] = [
      [`/_p/withdraw/${WITHDRAWAL_LINK}`, FETCH, "POST", 404],
      ["/_p/withdraw/declaration", FETCH, "GET", 404],
      ["/_p/withdraw/declaration", { ...FETCH, origin: "http://evil.example" }, "POST", 403],
    ];
    for (const [path, headers, method, status] of cases) {
      const res = await send(
        `${FRESH}${path}`,
        { ...headers, cookie, "x-page-token": token },
        method === "POST" ? { method, body: "{}" } : { method },
      );
      expect(res.status, `${method} ${path}`).toBe(status);
    }
    expect(calls).toEqual([]);
  });
});

describe("the review form posts _pt with the cookie grant (C4)", () => {
  const post = (fields: [string, string][], cookie = `__Host-Http-cap=${REVIEW}`, headers = FORM) =>
    send(
      `${FRESH}/review`,
      { ...headers, cookie },
      {
        method: "POST",
        body: new URLSearchParams(fields).toString(),
      },
    );
  const valid: [string, string][] = [
    ["_pt", tokenOf(REVIEW)],
    ["rating", "5"],
    ["name", "Jana N."],
    ["title", ""],
    ["body", "Sedí."],
  ];

  test("a valid review reaches the API with the grant and no link, then 303s to done", async () => {
    const res = await post([...valid, ["token", REVIEW_LINK]]);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("/review?done=1");
    expectPlatformHeaders(res, "success");
    const [call] = apiCalls("/link-grants/review");
    expect(call?.headers["x-link-grant"]).toBe(REVIEW);
    expect(JSON.parse(call?.body ?? "")).toEqual({
      rating: 5,
      name: "Jana N.",
      title: "",
      body: "Sedí.",
    });
  });

  test.each([
    [422, "invalid_body", "/review?error=invalid_body"],
    [429, "too_many_reviews", "/review?error=too_many_reviews"],
    [404, "not_found", "/review?link=invalid"],
    [500, "x", "/review?error=failed"],
  ])("a %i %s review 303s to %s", async (status, code, location) => {
    reviewAnswer = [status, code];
    const res = await post(valid);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(location);
    expectPlatformHeaders(res, location);
    expectNoSecret(res, location);
  });

  test.each([
    [
      "a stale page token",
      valid.map(([k, v]): [string, string] => [k, k === "_pt" ? tokenOf(OLD) : v]),
    ],
    ["no page token", valid.filter(([k]) => k !== "_pt")],
    ["a doubled field", [...valid, ["_pt", tokenOf(REVIEW)]] as [string, string][]],
  ])("%s reloads the page without submitting", async (_, fields) => {
    const res = await post(fields);
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe("/review");
    expect(calls).toEqual([]);
  });

  test("no grant cookie, a cross-origin form or a fetch submits nothing", async () => {
    expect((await post(valid, "")).headers.get("location")).toBe("/review");
    expect((await post(valid, undefined, { ...FORM, origin: "http://evil.example" })).status).toBe(
      403,
    );
    expect(
      (
        await post(valid, undefined, {
          ...FORM,
          "sec-fetch-mode": "cors",
          "sec-fetch-dest": "empty",
        })
      ).status,
    ).toBe(403);
    expect(calls).toEqual([]);
  });
});
