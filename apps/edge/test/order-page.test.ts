import { createHmac } from "node:crypto";
import { mkdtemp } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { afterAll, beforeAll, beforeEach, describe, expect, test } from "vitest";
import { createGateway, type Gateway } from "../src/gateway.ts";
import { StaticResolver } from "../src/sites.ts";
import {
  buildArtifact,
  fakeApi,
  LINK_GRANT,
  ORDER_ID,
  platformCheckoutWorker,
  site,
} from "./fixtures.ts";

const SECRET = "test-page-token-secret-0123456789abcdef";
const FRESH = "http://fresh.localhost:8280";
const DEMO = "http://demo.localhost:8280";
const CART = "sameorigintoken_00000000000000000042";
const PLANTED = "sameorigintoken_planted_000000000001";
const ATTEMPT = "11111111-2222-4333-8444-555555555555";
const STRIPE_SECRET = "pi_3Abc_secret_c0ffeec0ffee";
const tokenOf = (capability: string) =>
  createHmac("sha256", SECRET).update(`pt1|fresh.localhost:8280|${capability}`).digest("base64url");
const CART_TOKEN = tokenOf(CART);
const GRANT_TOKEN = tokenOf(LINK_GRANT);
const BOTH = `__Host-Http-cart=${CART}; __Host-Http-cap=${LINK_GRANT}`;
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
};
const FORM = {
  origin: FRESH,
  "sec-fetch-site": "same-origin",
  "sec-fetch-mode": "navigate",
  "sec-fetch-dest": "document",
  "content-type": "application/x-www-form-urlencoded",
};

let api: ReturnType<typeof fakeApi>;
let gw: Gateway;

const send = (
  url: string,
  headers: Record<string, string> = {},
  init: { method?: string; body?: string } = {},
) => gw.fetch(new Request(url, { ...init, headers: { host: new URL(url).host, ...headers } }));

const apiCalls = (prefix: string) =>
  api.calls.filter((c) => c.url.startsWith(`http://api.test/storefront/v1${prefix}`));

function expectPlatformHeaders(res: Response, label: string) {
  expect(res.headers.get("cache-control"), label).toBe("no-store");
  expect(res.headers.get("referrer-policy"), label).toBe("strict-origin");
  expect(res.headers.get("x-frame-options"), label).toBe("DENY");
  expect(res.headers.get("cross-origin-opener-policy"), label).toBe("same-origin");
}

beforeAll(async () => {
  const root = await mkdtemp(path.join(tmpdir(), "shop137-orders-"));
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
        checkout_origins: [
          { origin: FRESH, mode: "same_origin", mode_generation: 1, checkout_origin: FRESH },
        ],
      }),
      "demo.localhost": site(),
    }),
    checkoutArtifact: checkout.id,
    apiOrigin: "http://api.test",
    mediaOrigin: "http://media.test",
    scheme: "http",
    purgeToken: "purge-token-0123456789abcdef",
    upstream: api.fn,
    pageTokenSecret: SECRET,
  });
});

beforeEach(() => {
  api.calls.length = 0;
});

afterAll(async () => {
  await gw?.dispose();
});

describe("the clean order page renders by the grant or by the placing checkout (C4, C5)", () => {
  test("the placing browser's checkout capability alone opens it", async () => {
    const res = await send(`${FRESH}/o/${ORDER_ID}`, {
      ...NAVIGATE,
      cookie: `__Host-Http-cart=${CART}`,
    });
    expect(res.status).toBe(200);
    expect(await res.text()).not.toContain("open the link from your email again");
  });

  test("the render gets both candidate page tokens and every credential, the HTML neither cookie", async () => {
    const res = await send(`${FRESH}/o/${ORDER_ID}`, { ...NAVIGATE, cookie: BOTH });
    expect(res.status).toBe(200);
    const html = await res.text();
    const names = /<p id="headers">(.*?)<\/p>/.exec(html)?.[1]?.split(",") ?? [];
    expect(names).toContain("x-platform-page-token");
    expect(names).toContain("x-platform-grant-page-token");
    expect(html).not.toContain(LINK_GRANT);
    for (const read of apiCalls("/checkout")) {
      expect(read.headers["x-cart-token"]).toBe(CART);
      expect(read.headers["x-link-grant"]).toBe(LINK_GRANT);
    }
    expectPlatformHeaders(res, "order page");
  });
});

describe("the order page's API forwards only the credential its page token belongs to (C4)", () => {
  const ORDER = `${FRESH}/_p/orders/${ORDER_ID}`;
  const OPERATIONS: [string, string][] = [
    ["GET", ""],
    ["GET", "/payment"],
    ["POST", "/payment-attempts"],
    ["POST", `/payment-attempts/${ATTEMPT}/init`],
    ["POST", `/payment-attempts/${ATTEMPT}/simulate`],
  ];
  const call = (method: string, rest: string, headers: Record<string, string>) =>
    send(
      `${ORDER}${rest}`,
      { ...FETCH, "content-type": "application/json", ...headers },
      {
        method,
        ...(method === "POST" ? { body: "{}" } : {}),
      },
    );

  test.each(OPERATIONS)(
    "%s %s with the checkout's token sends the checkout capability only",
    async (method, rest) => {
      const res = await call(method, rest, { cookie: BOTH, "x-page-token": CART_TOKEN });
      expect(res.status, await res.clone().text()).toBe(200);
      const [forwarded] = apiCalls("/orders/");
      expect(forwarded?.url).toBe(`http://api.test/storefront/v1/orders/${ORDER_ID}${rest}`);
      expect(forwarded?.method).toBe(method);
      expect(forwarded?.headers["x-cart-token"]).toBe(CART);
      expect(forwarded?.headers["x-link-grant"]).toBeUndefined();
      expect(forwarded?.headers["x-shop-origin"]).toBe(FRESH);
      expect(res.headers.get("cache-control")).toBe("no-store");
    },
  );

  test("the grant's token sends the grant only", async () => {
    const res = await call("GET", "/payment", { cookie: BOTH, "x-page-token": GRANT_TOKEN });
    expect(res.status).toBe(200);
    const [forwarded] = apiCalls("/orders/");
    expect(forwarded?.headers["x-link-grant"]).toBe(LINK_GRANT);
    expect(forwarded?.headers["x-cart-token"]).toBeUndefined();
  });

  test("a planted capability with its own token never carries the customer's grant", async () => {
    const res = await call("GET", "", {
      cookie: `__Host-Http-cart=${PLANTED}; __Host-Http-cap=${LINK_GRANT}`,
      "x-page-token": tokenOf(PLANTED),
    });
    expect(res.status).toBe(200);
    const [forwarded] = apiCalls("/orders/");
    expect(forwarded?.headers["x-cart-token"]).toBe(PLANTED);
    expect(forwarded?.headers["x-link-grant"]).toBeUndefined();
  });

  test.each([
    ["no token", { cookie: BOTH }],
    ["a wrong token", { cookie: BOTH, "x-page-token": tokenOf("someone-else") }],
    ["no cookie", { "x-page-token": CART_TOKEN }],
    [
      "the grant's token without its cookie",
      { cookie: `__Host-Http-cart=${CART}`, "x-page-token": GRANT_TOKEN },
    ],
  ])("%s is refused before the API", async (_, headers) => {
    const res = await call("GET", "", headers);
    expect(res.status).toBe(403);
    expect(await res.text()).toBe("");
    expect(api.calls).toEqual([]);
  });

  test.each([
    ["documents", "GET", `/_p/orders/${ORDER_ID}/documents`],
    ["the bearer path", "GET", `/_p/orders/${"a".repeat(64)}`],
    ["an unknown operation", "DELETE", `/_p/orders/${ORDER_ID}`],
    ["a cancel", "POST", `/_p/orders/${ORDER_ID}/cancel`],
  ])("%s is not exposed to the page or a theme", async (_, method, route) => {
    const res = await send(
      `${FRESH}${route}`,
      { ...FETCH, cookie: BOTH, "x-page-token": CART_TOKEN },
      {
        method,
        ...(method === "POST" ? { body: "{}" } : {}),
      },
    );
    expect(res.status).toBe(404);
    expect(apiCalls("/orders/")).toEqual([]);
  });

  test("a cross-origin POST is refused", async () => {
    const res = await call("POST", "/payment-attempts", {
      cookie: BOTH,
      "x-page-token": CART_TOKEN,
      origin: "http://evil.localhost:8280",
      "sec-fetch-site": "cross-site",
    });
    expect(res.status).toBe(403);
    expect(apiCalls("/orders/")).toEqual([]);
  });

  test("an isolated shop has no order API on the shop origin", async () => {
    const res = await send(`${DEMO}/_p/orders/${ORDER_ID}`, { ...FETCH, origin: DEMO });
    expect(res.status).toBe(404);
    expect(api.calls).toEqual([]);
  });
});

describe("Stripe return parameters are consumed before any HTML (C5)", () => {
  const PAGE = `/o/${ORDER_ID}`;
  test.each([
    [
      "a succeeded redirect",
      `payment_intent=pi_3Abc&payment_intent_client_secret=${STRIPE_SECRET}&redirect_status=succeeded`,
    ],
    [
      "a failed redirect",
      `payment_intent=pi_3Abc&payment_intent_client_secret=${STRIPE_SECRET}&redirect_status=failed`,
    ],
    ["a processing redirect", `payment_intent=pi_3Abc&redirect_status=processing`],
    [
      "a setup intent",
      `setup_intent=seti_1&setup_intent_client_secret=${STRIPE_SECRET}&redirect_status=succeeded`,
    ],
    ["a source", `source=src_1&client_secret=${STRIPE_SECRET}`],
    ["a client secret alone", `client_secret=${STRIPE_SECRET}`],
    ["upper-case names", `Payment_Intent=pi_3Abc&REDIRECT_STATUS=succeeded`],
    [
      "repeated values",
      `redirect_status=succeeded&redirect_status=failed&payment_intent=pi_1&payment_intent=pi_2`,
    ],
    ["an empty value", "redirect_status="],
  ])("%s answers 303 to the clean page with only a return hint", async (_, query) => {
    const res = await send(`${FRESH}${PAGE}?${query}`, { ...NAVIGATE, cookie: BOTH });
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(`${PAGE}?returned=1`);
    expect(res.headers.get("cache-control")).toBe("no-store");
    expect(res.headers.getSetCookie()).toEqual([]);
    const seen = `${[...res.headers].join("\n")}${await res.text()}`;
    expect(seen).not.toContain(STRIPE_SECRET);
    expect(seen).not.toContain("pi_3Abc");
    expect(api.calls, "consumed at the edge: nothing reaches the API").toEqual([]);
  });

  test("other parameters stay, an email link parameter gives no return hint", async () => {
    const stripe = await send(`${FRESH}${PAGE}?utm_source=x&redirect_status=succeeded`, NAVIGATE);
    expect(stripe.headers.get("location")).toBe(`${PAGE}?utm_source=x&returned=1`);
    const link = await send(`${FRESH}${PAGE}?token=${"f".repeat(64)}`, NAVIGATE);
    expect(link.headers.get("location")).toBe(PAGE);
  });

  test.each([
    ["a fetch", FETCH, 403],
    ["a browser without Fetch Metadata", {}, 400],
  ])("%s with Stripe parameters is refused before the exchange", async (_, headers, status) => {
    const res = await send(`${FRESH}${PAGE}?client_secret=${STRIPE_SECRET}`, headers);
    expect(res.status).toBe(status);
    expect(res.headers.get("location")).toBeNull();
    expect(await res.text()).not.toContain(STRIPE_SECRET);
  });

  test("a noncanonical path with Stripe parameters answers 404 without Location", async () => {
    const res = await send(`${FRESH}/checkout/?client_secret=${STRIPE_SECRET}`, NAVIGATE);
    expect(res.status).toBe(404);
    expect(res.headers.get("location")).toBeNull();
    expect(await res.text()).not.toContain(STRIPE_SECRET);
  });
});

describe("fake pay on the shop origin is a platform page with the payer's token (C1, C4)", () => {
  const PAGE = `${FRESH}/_p/fake-pay/${ATTEMPT}?return=/o/${ORDER_ID}`;
  const fakePayCalls = () => apiCalls(`/checkout/fake-pay/${ATTEMPT}`);

  test("an admitted navigation shows the page with the checkout's token in its form", async () => {
    const res = await send(PAGE, { ...NAVIGATE, cookie: BOTH });
    expect(res.status).toBe(200);
    const html = await res.text();
    expect(html).toContain(`name="_pt" value="${CART_TOKEN}"`);
    expect(html).toContain(`action="/_p/fake-pay/${ATTEMPT}?return=/o/${ORDER_ID}"`);
    expect(html).toContain("&lt;b&gt;100001&lt;/b&gt;");
    expect(html).not.toContain(CART);
    expect(html).not.toContain(LINK_GRANT);
    const [read] = fakePayCalls();
    expect(read?.headers["x-cart-token"]).toBe(CART);
    expect(read?.headers["x-link-grant"]).toBeUndefined();
    expect(read?.headers["x-shop-origin"]).toBe(FRESH);
    expectPlatformHeaders(res, "fake pay");
  });

  test.each([
    ["a fetch", FETCH, 403],
    ["a prefetch", { ...NAVIGATE, "sec-purpose": "prefetch" }, 403],
    ["an iframe", { ...NAVIGATE, "sec-fetch-dest": "iframe" }, 403],
  ])("%s is refused before the API", async (_, headers, status) => {
    const res = await send(PAGE, { ...headers, cookie: BOTH });
    expect(res.status).toBe(status);
    expect(fakePayCalls()).toEqual([]);
  });

  test("a browser without the checkout capability gets no page", async () => {
    const res = await send(PAGE, { ...NAVIGATE, cookie: `__Host-Http-cap=${LINK_GRANT}` });
    expect(res.status).toBe(403);
    expect(fakePayCalls()).toEqual([]);
  });

  test("the outcome form pays with the token and returns to the clean order page", async () => {
    const res = await send(
      PAGE,
      { ...FORM, cookie: BOTH },
      {
        method: "POST",
        body: `_pt=${encodeURIComponent(CART_TOKEN)}&outcome=failed`,
      },
    );
    expect(res.status).toBe(303);
    expect(res.headers.get("location")).toBe(`/o/${ORDER_ID}`);
    const [paid] = fakePayCalls();
    expect(paid?.method).toBe("POST");
    expect(JSON.parse(paid?.body ?? "")).toEqual({ outcome: "failed" });
    expect(paid?.headers["x-cart-token"]).toBe(CART);
  });

  test.each([
    ["no token", "outcome=succeeded", FORM],
    ["the grant's token", `_pt=${encodeURIComponent(GRANT_TOKEN)}&outcome=succeeded`, FORM],
    [
      "a fetch",
      `_pt=${encodeURIComponent(CART_TOKEN)}&outcome=succeeded`,
      { ...FORM, "sec-fetch-mode": "cors", "sec-fetch-dest": "empty" },
    ],
    [
      "a cross-site form",
      `_pt=${encodeURIComponent(CART_TOKEN)}&outcome=succeeded`,
      { ...FORM, origin: "http://evil.localhost:8280", "sec-fetch-site": "cross-site" },
    ],
  ])("%s cannot pay", async (_, body, headers) => {
    const res = await send(PAGE, { ...headers, cookie: BOTH }, { method: "POST", body });
    expect(res.status).toBe(403);
    expect(fakePayCalls()).toEqual([]);
  });

  test.each([`/o/${"a".repeat(64)}`, "//evil.example/o/x", "/checkout"])(
    "return %s is not followed",
    async (back) => {
      const res = await send(
        `${FRESH}/_p/fake-pay/${ATTEMPT}?return=${back}`,
        { ...FORM, cookie: BOTH },
        {
          method: "POST",
          body: `_pt=${encodeURIComponent(CART_TOKEN)}&outcome=succeeded`,
        },
      );
      expect(res.headers.get("location")).toBe("/");
    },
  );
});
