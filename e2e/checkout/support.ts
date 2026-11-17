/**
 * Shared checkout-origin helpers for the checkout e2e suites (WP10 orders, WP11 payments):
 * a shop cart handed to checkout, the address form, the pickup-point widget, placing the
 * order, the fake gateway and the order model.
 */

import { createHash, randomBytes } from "node:crypto";
import {
  type Browser,
  type BrowserContext,
  type BrowserContextOptions,
  expect,
  type Locator,
  type Page,
  type Request,
} from "@playwright/test";
import { expectAccessible, mailpit } from "../admin/support";
import { tabTo } from "../keyboard";
import { rateHeaders, testContext, testRateHeaders } from "../rate-client";

const port = process.env.HTTP_PORT ?? "8080";
export const CZ = `http://demo.localhost:${port}`;
export const SK = `http://demo-sk.localhost:${port}`;
export const FRESH = new URL(`http://demo-fresh.localhost:${port}`).origin;
/** WebKit drops `Secure` cookies over plain HTTP, so the security projects use this origin. */
export const FRESH_HTTPS = new URL(
  `https://demo-fresh.localhost:${process.env.HTTPS_PORT ?? "8443"}`,
).origin;
export const FRESH_SK = new URL(`http://demo-fresh-sk.localhost:${port}`).origin;
export const FRESH_SK_HTTPS = new URL(
  `https://demo-fresh-sk.localhost:${process.env.HTTPS_PORT ?? "8443"}`,
).origin;
export const checkoutOf = (shop: string) => shop.replace("://", "://checkout.");
/** The demo shop's account origins (SHOP-95, A1): `<slug>.<ACCOUNT_DOMAIN>`, `<market>--<slug>.…`. */
export const ACCOUNT = `http://demo.accounts.localhost:${port}`;
export const ACCOUNT_SK = `http://sk--demo.accounts.localhost:${port}`;

export const RESTARTED =
  "Pokladna byla mezitím spuštěna znovu nebo vypršela. Pokračujte s aktuálním košíkem.";
export const restartNotice = (page: Page) =>
  page.getByRole("status").filter({ hasText: "Pokladna byla mezitím spuštěna znovu" });
export const restartButton = (page: Page) =>
  page.getByRole("button", { name: "Pokračovat k pokladně" });
export const pageTokenMeta = (page: Page) => page.locator('meta[name="page-token"]');

export async function securityContext(
  browser: Browser,
  options: BrowserContextOptions = {},
): Promise<BrowserContext> {
  const context = await browser.newContext({
    ignoreHTTPSErrors: true,
    locale: "cs-CZ",
    extraHTTPHeaders: testRateHeaders(),
    ...options,
  });
  await context.addCookies(
    [FRESH_HTTPS, FRESH_SK_HTTPS].map((url) => ({ name: "consent", value: "", url })),
  );
  return context;
}

/**
 * Another tab on a shop the context already shows. Playwright's Firefox driver can drop the commit
 * of a new tab whose first navigation is a COOP page joining the shop's process, which hangs
 * `goto` at every waitUntil; landing on a COOP-less file of the shop first avoids that swap.
 */
export async function shopTab(context: BrowserContext, url: string): Promise<Page> {
  const tab = await context.newPage();
  await tab.goto(`${new URL(url).origin}/favicon.svg`);
  await tab.goto(url);
  return tab;
}

export async function cookieValue(
  context: BrowserContext,
  name: string,
): Promise<string | undefined> {
  return (await context.cookies()).find((c) => c.name === name)?.value;
}

/** SSR controls are visible before the checkout island attaches its event handlers. */
export async function checkoutReady(page: Page): Promise<void> {
  await page.waitForFunction(() => !document.querySelector('astro-island[client="load"][ssr]'));
}

/** Hold island modules to exercise visible SSR controls before event handlers attach. */
export async function deferHydration(page: Page): Promise<() => void> {
  let release!: () => void;
  const scripts = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route(/\.(?:js|mjs)(?:\?|$)/, async (route) => {
    await scripts;
    await route.fallback();
  });
  return release;
}

export interface Money {
  amount_minor: number;
  formatted: string;
}
export interface CartModel {
  subtotal: Money;
  discount: Money;
  total: Money;
}
export interface OrderModel {
  number: string;
  status: string;
  subtotal: Money;
  discount: Money;
  shipping_total: Money;
  total: Money;
  payment: { status: string };
}

export async function newPage(browser: Browser, locale = "cs-CZ"): Promise<Page> {
  const ctx = await testContext(browser, { locale });
  // A decided consent keeps the theme's banner closed.
  await ctx.addCookies([
    { name: "consent", value: "", url: CZ },
    { name: "consent", value: "", url: SK },
  ]);
  return ctx.newPage();
}

export async function addLine(page: Page, shop: string, slug: string, quantity = 1) {
  const model = (await (
    await page.request.get(`${shop}/_p/public/pages/product/${slug}`, {
      headers: rateHeaders(page),
    })
  ).json()) as {
    product: { variants: { id: string }[] };
  };
  // Every placed order reserves stock (A13): take the first variant that still has enough.
  let added = 0;
  for (const v of model.product.variants) {
    const res = await page.request.post(`${shop}/_p/cart/lines`, {
      headers: { origin: shop, ...rateHeaders(page) },
      data: { variant_id: v.id, quantity },
    });
    added = res.status();
    if (added !== 409) break;
  }
  expect(added).toBe(200);
}

export function submitStart(page: Page): Promise<void> {
  return page.evaluate(() => {
    const form = document.createElement("form");
    form.method = "post";
    form.action = "/_p/checkout/start";
    document.body.append(form);
    form.submit();
  });
}

export async function startSameOriginCheckout(page: Page, shop: string, slug = "tricko-basic") {
  await page.goto(`${shop}/`);
  await addLine(page, shop, slug);
  await Promise.all([page.waitForURL(`${shop}/checkout`), submitStart(page)]);
}

type DetailsStep = "contact" | "addresses";
export const detailsForm = (page: Page, step: DetailsStep) =>
  page.locator(`form[action="/checkout/${step}"]`);

export async function postDetails(
  page: Page,
  step: DetailsStep,
  submit?: () => Promise<void>,
): Promise<number> {
  const posted = page.waitForRequest(
    (r) => r.method() === "POST" && new URL(r.url()).pathname === `/checkout/${step}`,
  );
  const committed = page.waitForEvent("framenavigated", (frame) => frame === page.mainFrame());
  await (submit ? submit() : detailsForm(page, step).getByRole("button").click());
  const status = (await (await posted).response())?.status() ?? 0;
  await committed;
  return status;
}

export interface AddressInput {
  name: string;
  street: string;
  postal_code: string;
  city: string;
}
export const JANA: AddressInput = {
  name: "Jana Nováková",
  street: "Dlouhá 12",
  postal_code: "110 00",
  city: "Praha",
};

export async function fillAddress(page: Page, prefix: "billing" | "shipping", a: AddressInput) {
  const form = detailsForm(page, "addresses");
  for (const [key, value] of Object.entries(a))
    await form.locator(`[name="${prefix}_${key}"]`).fill(value);
}

export async function saveContact(page: Page, email: string) {
  await detailsForm(page, "contact").locator('[name="email"]').fill(email);
  expect(await postDetails(page, "contact"), "the contact post").toBe(303);
}

export async function saveAddresses(page: Page, billing = JANA) {
  await fillAddress(page, "billing", billing);
  expect(await postDetails(page, "addresses"), "the address post").toBe(303);
}

export async function sameOriginReview(page: Page, shop: string, email: string, slug?: string) {
  await startSameOriginCheckout(page, shop, slug);
  await saveContact(page, email);
  await saveAddresses(page);
  await checkoutReady(page);
}

export async function chooseCashOnDelivery(page: Page, shipping = /PPL – kurýr/, cod = /Dobírka/) {
  const save = async (path: "shipping" | "payment", choice: Locator) => {
    const [saved] = await Promise.all([
      page.waitForResponse(
        (r) =>
          r.request().method() === "PUT" && new URL(r.url()).pathname === `/_p/checkout/${path}`,
      ),
      choice.check(),
    ]);
    expect(saved.status(), `the ${path} save`).toBe(200);
  };
  await save("shipping", page.getByRole("radio", { name: shipping }));
  const pay = page.getByRole("radio", { name: cod });
  await expect(pay).toBeEnabled();
  await save("payment", pay);
  await expect(pay).toBeChecked();
}

export async function receiptNumber(page: Page, thanks = "Děkujeme za objednávku!") {
  const receipt = page.getByRole("status", { name: thanks });
  const orderPage = page.getByRole("heading", { level: 1, name: /^Objednávka \S+$/ });
  await expect(receipt.or(orderPage)).toBeVisible();
  if (await orderPage.isVisible()) {
    await expect(page.getByText(thanks)).toBeVisible();
    return /^Objednávka (\S+)$/.exec(await orderPage.innerText())?.[1] ?? "";
  }
  await expect(receipt).toBeFocused();
  const number = /Objednávka (\S+)/.exec(await receipt.innerText())?.[1];
  expect(number, "the receipt names the order").toBeTruthy();
  return number ?? "";
}

/** Fills a shop cart through the edge (optionally with a coupon) and hands it to checkout. */
export async function toCheckout(
  page: Page,
  shop: string,
  slug: string,
  quantity = 1,
  coupon?: string,
): Promise<CartModel> {
  await page.goto(`${shop}/`);
  await addLine(page, shop, slug, quantity);
  if (coupon) {
    const applied = await page.request.post(`${shop}/_p/cart/coupons`, {
      headers: { origin: shop, ...rateHeaders(page) },
      data: { code: coupon },
    });
    expect(applied.status()).toBe(200);
  }
  const cart = (await (
    await page.request.get(`${shop}/_p/cart`, { headers: rateHeaders(page) })
  ).json()) as CartModel;
  await Promise.all([page.waitForURL(`${checkoutOf(shop)}/`), submitStart(page)]);
  await checkoutReady(page);
  return cart;
}

export async function fillContactAndAddress(page: Page, email: string | null, city = "Praha") {
  await checkoutReady(page);
  if (email !== null) await page.locator('input[autocomplete="email"]').fill(email);
  await page.locator('input[autocomplete="name"]').fill("Jana Nováková");
  await page.locator('input[autocomplete="street-address"]').fill("Dlouhá 12");
  await page.locator('input[autocomplete="postal-code"]').fill("110 00");
  await page.locator('input[autocomplete="address-level2"]').fill(city);
}

/** Chooses the Packeta pickup method and a point in the (mock) widget. */
export async function choosePickupPoint(page: Page, point: RegExp, keyboard = false) {
  await page.getByRole("radio", { name: /Zásilkovna – výdejní místo/ }).check();
  const widget = page.frameLocator('iframe[title="Packeta"]');
  const target = widget.getByRole("button", { name: point });
  await expect(target).toBeVisible();
  // A modal: the checkout behind it is inert (out of the tab order) while it is open.
  await expect(page.locator("main")).toHaveJSProperty("inert", true);
  if (keyboard) {
    // Tab cycles inside the dialog; Enter chooses (A26: keyboard pickup-point selection).
    const first = widget.getByRole("button").first();
    await tabTo(page, first);
    await page.keyboard.press("Shift+Tab");
    await expect(widget.getByRole("button", { name: "Zavřít" })).toBeFocused();
    await page.keyboard.press("Tab");
    await expect(first).toBeFocused();
    await tabTo(page, target);
    await page.keyboard.press("Enter");
  } else {
    // The widget removes its frame right after the choice; a plain click would wait on it.
    await target.dispatchEvent("click");
  }
  await expect(page.getByTestId("pickup-point")).toContainText(point);
}

export const isPlacement = (r: Request) =>
  r.method() === "POST" && new URL(r.url()).pathname === "/checkout/place-order";

export async function acceptAndPlace(page: Page, placeLabel = "Objednat s povinností platby") {
  for (const label of [/^(Souhlasím|Súhlasím) s obchodn/, /^Ber(u|iem) na v[ěe]dom/]) {
    const box = page.getByRole("checkbox", { name: label });
    if (!(await box.isChecked())) await page.getByText(label).click();
    await expect(box).toBeChecked();
  }
  await page.getByRole("button", { name: placeLabel }).click();
}

/** On the fake gateway's page: pay or fail; returns the order token from the order page. */
export async function fakePay(page: Page, button: "Pay" | "Fail the payment"): Promise<string> {
  await page.waitForURL(/\/_p\/fake-pay\//);
  await expect(page.getByRole("heading", { name: "Test payment" })).toBeVisible();
  await page.getByRole("button", { name: button, exact: true }).click();
  await page.waitForURL(/\/o\/[0-9a-f]{64}$/);
  return new URL(page.url()).pathname.slice(3);
}

export async function order(page: Page, shop: string, token: string): Promise<OrderModel> {
  return (await (
    await page.request.get(`${checkoutOf(shop)}/_p/orders/${token}`, {
      headers: rateHeaders(page),
    })
  ).json()) as OrderModel;
}

export interface MailSummary {
  ID: string;
  Subject: string;
  Created: string;
}

/** The newest email to `to` whose subject contains `subject` (polls Mailpit). */
export async function mail(to: string, subject: string): Promise<{ Text: string; HTML: string }> {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    const res = await fetch(`${mailpit}/api/v1/search?query=${encodeURIComponent(`to:"${to}"`)}`);
    const body = (await res.json()) as { messages: MailSummary[] };
    const hit = body.messages.find((m) => m.Subject.includes(subject));
    if (hit)
      return (await (await fetch(`${mailpit}/api/v1/message/${hit.ID}`)).json()) as {
        Text: string;
        HTML: string;
      };
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`no email "${subject}" to ${to}`);
}

export const CLEAN_ORDER = /\/o\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/;

export async function sameOriginOrder(page: Page, email: string): Promise<string> {
  await sameOriginReview(page, FRESH_HTTPS, email);
  await chooseCashOnDelivery(page);
  await acceptAndPlace(page);
  await page.waitForURL(CLEAN_ORDER);
  return CLEAN_ORDER.exec(new URL(page.url()).pathname)?.[1] ?? "";
}

export const historyTraces = (page: Page) =>
  page.evaluate(() => {
    const nav = (window as unknown as { navigation?: { entries(): { url: string | null }[] } })
      .navigation;
    return {
      entries: nav ? nav.entries().map((e) => e.url ?? "") : [location.href],
      referrer: document.referrer,
      html: document.documentElement.outerHTML,
    };
  });

export const themeRequests = (page: Page, requests: [string, string][]) =>
  page.evaluate(
    async (list) =>
      Promise.all(
        list.map(async ([method, url]) => {
          const res = await fetch(url, {
            method,
            credentials: "include",
            ...(method === "POST"
              ? { headers: { "content-type": "application/json" }, body: "{}" }
              : {}),
          });
          return `${method} ${url} ${res.status} ${await res.text()}`;
        }),
      ),
    requests,
  );

export function redirectLocations(page: Page): string[] {
  const seen: string[] = [];
  page.on("response", (r) => {
    const location = r.headers().location;
    if (location) seen.push(location);
  });
  return seen;
}

export async function expectGrantPage(
  page: Page,
  token: string,
  redirects: string[],
  action: Locator,
  name: string,
) {
  await expect(page.locator('input[name="_pt"]')).toHaveCount(1);
  await expect(page.locator('input[name="token"], input[name="t"]')).toHaveCount(0);
  const traces = await historyTraces(page);
  for (const trace of [...traces.entries, traces.referrer, traces.html, ...redirects])
    expect(trace, "no emailed link in history, referrer, HTML or redirects").not.toContain(token);
  for (const width of [390, 1280]) {
    await page.setViewportSize({ width, height: 844 });
    await expect(action, `${name} at ${width}px`).toBeInViewport();
    expect(
      await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth),
      `${name} has no sideways scroll at ${width}px`,
    ).toBe(true);
  }
  await page.setViewportSize({ width: 390, height: 844 });
  await expectAccessible(page, name);
}

export async function postForm(
  page: Page,
  path: string,
  submit: () => Promise<void>,
): Promise<string> {
  const posted = page.waitForResponse(
    (r) => r.request().method() === "POST" && new URL(r.url()).pathname === path,
  );
  const committed = page.waitForEvent("framenavigated", (frame) => frame === page.mainFrame());
  await submit();
  const res = await posted;
  expect(res.status(), `POST ${path}`).toBe(303);
  await committed;
  await page.waitForLoadState("load");
  return res.headers().location ?? "";
}

export function linkToken(): { token: string; hash: string } {
  const token = randomBytes(32).toString("hex");
  return { token, hash: createHash("sha256").update(token).digest("hex") };
}
