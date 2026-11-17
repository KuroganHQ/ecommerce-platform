/**
 * SHOP-149: an owner records a checkout origin's history and switches its checkout mode from the
 * admin, on a dedicated tenant. Every switch ends the checkouts opened before it. The toggle needs
 * an API started with `SAME_ORIGIN_CHECKOUT_READY=1`; without it the switch is refused instead.
 */

import {
  type APIRequestContext,
  type Browser,
  type BrowserContext,
  expect,
  type Page,
  test,
} from "@playwright/test";
import { checkoutOf, submitStart } from "../checkout/support";
import { tabTo } from "../keyboard";
import { rateHeaders, testContext } from "../rate-client";
import { createTenant, env, expectAccessible, magicLink, run, useEnglish } from "./support.ts";

test.describe.configure({ mode: "serial" });

const READY = env("SAME_ORIGIN_CHECKOUT_READY", "0") === "1";
const slug = `co-${run}`;
const owner = `co-owner-${run}@example.test`;
const origin = `https://${slug}.localhost`;
const ATTESTATION =
  "I know the full history of this exact web address. It never served a website outside this platform, under me or an earlier owner.";
const RESTART =
  "It ends every checkout in progress on the shop, on all its domains: open checkouts, link grants, identify and placement states stop working in the switch's transaction, and customers start checkout again. Emailed order links keep working.";
const ROLLBACK = `Provision and verify DNS and TLS for checkout.${slug}.localhost first, or isolated checkout has no host.`;

let browser: Browser;
let context: BrowserContext;
let page: Page;
let shop: string;
let variant: string;

const row = () => page.getByRole("row").filter({ hasText: origin });
const dialog = () => page.getByRole("dialog");

async function adminApi(request: APIRequestContext) {
  const token: string = await page.evaluate(async () => {
    const res = await fetch("/api/auth/token", { credentials: "same-origin" });
    return ((await res.json()) as { token: string }).token;
  });
  const tenant = await page.evaluate(() => localStorage.getItem("admin.tenant") ?? "");
  const base = `http://api.localhost:${new URL(page.url()).port}/admin/v1`;
  const headers = { authorization: `Bearer ${token}`, "x-tenant-id": tenant };
  return {
    get: (path: string) => request.get(`${base}${path}`, { headers }),
    post: (path: string, data: unknown) => request.post(`${base}${path}`, { headers, data }),
    put: (path: string, data: unknown) => request.put(`${base}${path}`, { headers, data }),
  };
}

/** A sellable product: tax profile, a CZK price list, one active priced variant in stock. */
async function sellable(request: APIRequestContext): Promise<string> {
  const api = await adminApi(request);
  const markets = (await (await api.get("/markets")).json()) as {
    items: { id: string; code: string }[];
  };
  const cz = markets.items.find((m) => m.code === "cz")?.id ?? "";
  const tax = await api.put("/tax-profile", {
    establishment_country: "CZ",
    vat_payer: true,
    vat_id: "CZ12345678",
    sk_ic_dph: null,
    distance_sales_mode: "destination",
  });
  expect(tax.status()).toBe(200);
  const list = await api.post("/price-lists", {
    code: "czk",
    name: "CZK",
    currency: "CZK",
    market_ids: [cz],
  });
  expect(list.status()).toBe(201);
  const listId = ((await list.json()) as { id: string }).id;
  const product = await api.post("/products", {
    status: "active",
    translations: [{ locale: "cs", name: "Tričko", slug: `tricko-${run}` }],
    variants: [{ sku: `CO-${run}` }],
  });
  expect(product.status()).toBe(201);
  const id = ((await product.json()) as { variants: { id: string }[] }).variants[0]?.id ?? "";
  const price = await api.put(`/price-lists/${listId}/prices`, {
    items: [{ variant_id: id, amount_minor: 19900 }],
  });
  expect(price.status()).toBe(200);
  const stock = await api.post(`/inventory/${id}/adjustments`, { on_hand: 10 });
  expect(stock.status()).toBe(201);
  return id;
}

/** A shopper's isolated checkout, opened through the shop's handoff; the probe reads it back. */
async function openCheckout(): Promise<() => Promise<number>> {
  const shopper = await (await testContext(browser)).newPage();
  await shopper.goto(`${shop}/`);
  const added = await shopper.request.post(`${shop}/_p/cart/lines`, {
    headers: { origin: shop, ...rateHeaders(shopper) },
    data: { variant_id: variant, quantity: 1 },
  });
  expect(added.status()).toBe(200);
  await Promise.all([shopper.waitForURL(`${checkoutOf(shop)}/`), submitStart(shopper)]);
  const probe = async () =>
    (
      await shopper.request.get(`${checkoutOf(shop)}/_p/checkout`, {
        headers: rateHeaders(shopper),
      })
    ).status();
  expect(await probe(), "the checkout is open").toBe(200);
  return probe;
}

async function switchMode(to: "Switch to same-origin" | "Switch to isolated") {
  const before = Number(await row().getByRole("cell").nth(2).textContent());
  await row().getByRole("button", { name: to }).click();
  await expect(dialog()).toContainText(RESTART);
  if (to === "Switch to isolated") await expect(dialog()).toContainText(ROLLBACK);
  else await expect(dialog()).not.toContainText(ROLLBACK);
  const [res] = await Promise.all([
    page.waitForResponse((r) => r.url().endsWith("/admin/v1/checkout-origins/mode")),
    dialog().getByRole("button", { name: to }).click(),
  ]);
  expect(res.status()).toBe(200);
  await expect(dialog()).toBeHidden();
  await expect(row().getByRole("cell").nth(2)).toHaveText(String(before + 1));
}

test.beforeAll(async ({ browser: b }) => {
  browser = b;
  createTenant(slug, `Checkout origins ${run}`, owner);
  context = await testContext(b);
  page = await context.newPage();
  await useEnglish(page);
  await page.goto("/login");
  const since = new Date(Date.now() - 1000);
  await page.getByLabel("Email").fill(owner);
  await page.getByRole("button", { name: "Email me a sign-in link" }).click();
  await page.goto(await magicLink(owner, since));
  await expect(page.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
  shop = `http://${slug}.localhost:${new URL(page.url()).port}`;
});
test.afterAll(async () => {
  await context?.close();
});

test("owner records the platform history; an attestation on a platform hostname is refused", async () => {
  await page
    .getByRole("navigation", { name: "Main navigation" })
    .getByRole("link", { name: "Checkout domains", exact: true })
    .click();
  await expect(page.getByRole("heading", { name: "Checkout domains", level: 1 })).toBeVisible();
  await expect(row()).toContainText("Isolated");
  await expect(row()).toContainText("Unknown");
  await expectAccessible(page, "checkout-origins");

  await row().getByRole("button", { name: "Record history" }).click();
  const box = dialog().getByRole("checkbox", { name: ATTESTATION });
  await tabTo(page, box);
  await page.keyboard.press("Space");
  await expect(box).toBeChecked();
  await tabTo(page, dialog().getByRole("button", { name: "Record history" }));
  await page.keyboard.press("Enter");
  await expect(dialog().getByRole("alert")).toHaveText(
    "A platform hostname's history is the platform's own; it takes no attestation.",
  );
  await expectAccessible(page, "checkout-origins-history-refused");

  await tabTo(page, box);
  await page.keyboard.press("Space");
  await expect(box).not.toBeChecked();
  await dialog().getByRole("button", { name: "Record history" }).click();
  await expect(dialog()).toBeHidden();
  await expect(row()).toContainText("Platform address");
  await expect(row()).toContainText("Isolated");
});

test("an unready platform refuses the switch to same-origin", async () => {
  test.skip(READY, "the API runs with SAME_ORIGIN_CHECKOUT_READY=1");
  await row().getByRole("button", { name: "Switch to same-origin" }).click();
  await expect(dialog()).toContainText(RESTART);
  await dialog().getByRole("button", { name: "Switch to same-origin" }).click();
  await expect(dialog().getByRole("alert")).toHaveText(
    "The platform is not ready for same-origin checkout.",
  );
  await dialog().getByRole("button", { name: "Cancel" }).click();
  await expect(row()).toContainText("Isolated");
});

test("isolated -> same-origin -> isolated, each switch revoking the open checkout", async ({
  request,
}) => {
  test.skip(!READY, "needs an API started with SAME_ORIGIN_CHECKOUT_READY=1");
  variant = await sellable(request);

  const first = await openCheckout();
  await switchMode("Switch to same-origin");
  await expect(row()).toContainText("Same-origin");
  expect(await first(), "the checkout from before the switch is revoked").toBe(404);

  const second = await openCheckout();
  await switchMode("Switch to isolated");
  await expect(row()).toContainText("Isolated");
  expect(await second(), "the checkout from before the rollback is revoked").toBe(404);
});
