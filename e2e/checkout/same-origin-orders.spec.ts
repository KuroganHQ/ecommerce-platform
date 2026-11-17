/**
 * SHOP-137 (SHOP-54 slice 10): on a same-origin shop the order page is `/o/<order id>`. The
 * placing browser reads and pays through its checkout capability, an emailed link reads through
 * a scoped grant, and no URL, history entry or referrer ever holds a secret (C1, C4, C5, C7).
 */

import { expect, type Page, test } from "@playwright/test";
import { run } from "../admin/support";
import {
  acceptAndPlace,
  chooseCashOnDelivery,
  choosePickupPoint,
  FRESH_HTTPS,
  mail,
  pageTokenMeta,
  sameOriginReview,
  securityContext,
} from "./support";

const CLEAN_ORDER = /\/o\/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})$/;
const REOPEN = "Otevřete prosím odkaz z e-mailu znovu";

/** Every URL the tab's history exposes to same-origin script (Navigation API), and the referrer. */
const historyTraces = (page: Page) =>
  page.evaluate(() => {
    const nav = (window as unknown as { navigation?: { entries(): { url: string | null }[] } })
      .navigation;
    return {
      entries: nav ? nav.entries().map((e) => e.url ?? "") : [location.href],
      referrer: document.referrer,
      html: document.documentElement.outerHTML,
    };
  });

async function placed(page: Page): Promise<string> {
  await acceptAndPlace(page);
  await page.waitForURL(CLEAN_ORDER);
  return CLEAN_ORDER.exec(new URL(page.url()).pathname)?.[1] ?? "";
}

async function fakePay(page: Page, button: "Pay" | "Fail the payment", id: string) {
  await page.waitForURL(/\/_p\/fake-pay\//);
  expect(new URL(page.url()).searchParams.get("return")).toBe(`/o/${id}`);
  await expect(page.locator('input[name="_pt"]')).toHaveCount(1);
  await page.getByRole("button", { name: button, exact: true }).click();
  await page.waitForURL(`${FRESH_HTTPS}/o/${id}`);
}

const theme = (page: Page, paths: string[]) =>
  page.evaluate(
    async (urls) =>
      Promise.all(
        urls.map(async (url) => {
          const res = await fetch(url, { credentials: "include" });
          return { url, status: res.status, body: await res.text() };
        }),
      ),
    paths,
  );

test("same-origin order: the payer fails, retries and pays; the email link only reads", async ({
  browser,
}) => {
  const email = `so-order-${run}@example.test`;
  const payer = await securityContext(browser);
  const page = await payer.newPage();
  await sameOriginReview(page, FRESH_HTTPS, email);
  await chooseCashOnDelivery(page, /PPL – kurýr/, /Testovací platba/);
  const id = await placed(page);

  await expect(page.getByText("Děkujeme za objednávku!")).toBeVisible();
  await expect(page.getByText(email).first()).toBeVisible();
  await expect(pageTokenMeta(page), "the payer's page token").toHaveCount(1);
  await page.getByRole("button", { name: "Pokračovat k platbě" }).click();
  await fakePay(page, "Fail the payment", id);
  await expect(page.getByTestId("payment-status")).toHaveText("Platba se nezdařila");
  const attempt = await page.evaluate(async (order) => {
    const token = document.querySelector<HTMLMetaElement>('meta[name="page-token"]')?.content;
    const res = await fetch(`/_p/orders/${order}/payment`, {
      headers: { "x-page-token": token ?? "" },
    });
    return ((await res.json()) as { attempt?: { id: string } }).attempt?.id ?? "";
  }, id);

  const confirmation = await mail(email, "Potvrzení objednávky");
  const link = /\/o\/([0-9a-f]{64})/.exec(confirmation.Text)?.[1] ?? "";
  expect(link).toMatch(/^[0-9a-f]{64}$/);

  const other = await securityContext(browser);
  const reader = await other.newPage();
  const stranger = await reader.goto(`${FRESH_HTTPS}/o/${id}`);
  expect(stranger?.status(), "the clean URL in a browser with no grant").toBe(404);
  await expect(reader.getByText(REOPEN)).toBeVisible();
  expect(await reader.content(), "no order data for another browser").not.toContain(email);

  await reader.goto(`${FRESH_HTTPS}/o/${link}`);
  await reader.waitForURL(`${FRESH_HTTPS}/o/${id}`);
  await expect(reader.getByTestId("payment-status")).toHaveText("Platba se nezdařila");
  await expect(reader.getByText(email).first()).toBeVisible();
  await expect(reader.getByRole("button", { name: "Zaplatit znovu" })).toHaveCount(0);
  const traces = await historyTraces(reader);
  expect(traces.entries.length, "the Navigation API lists the tab's entries").toBeGreaterThan(1);
  for (const seen of [...traces.entries, traces.referrer, traces.html])
    expect(seen, "the email link is in no history entry, referrer or HTML").not.toContain(link);
  const bearerRetry = await reader.evaluate(async (order) => {
    const token = document.querySelector<HTMLMetaElement>('meta[name="page-token"]')?.content;
    const res = await fetch(`/_p/orders/${order}/payment-attempts`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-page-token": token ?? "" },
      body: "{}",
    });
    return { status: res.status, body: await res.text() };
  }, id);
  expect(bearerRetry.status, "the grant alone does not pay").toBe(403);
  expect(bearerRetry.body).toContain("payment_not_allowed");
  const gateway = await reader.goto(`${FRESH_HTTPS}/_p/fake-pay/${attempt}?return=/o/${id}`);
  expect(gateway?.status(), "fake pay needs the placing checkout").toBe(403);

  await reader.goto(`${FRESH_HTTPS}/`);
  const stolen = await theme(reader, [
    `/o/${id}`,
    `/_p/orders/${id}`,
    `/_p/orders/${id}/payment`,
    `/_p/orders/${id}/documents`,
  ]);
  for (const r of stolen) {
    expect([403, 404], `a theme fetch of ${r.url}`).toContain(r.status);
    expect(r.body, r.url).not.toContain(email);
  }
  await other.close();

  await page.getByRole("button", { name: "Zaplatit znovu" }).click();
  await fakePay(page, "Pay", id);
  await expect(page.getByTestId("payment-status")).toHaveText("Zaplaceno");
  await expect(page.getByTestId("order-status")).toHaveText("Potvrzená");
  await payer.close();
});

test("a Stripe return to the order page and a click on the shop name leave no secret behind", async ({
  browser,
}) => {
  const context = await securityContext(browser);
  const page = await context.newPage();
  await sameOriginReview(page, FRESH_HTTPS, `so-return-${run}@example.test`);
  await chooseCashOnDelivery(page);
  const id = await placed(page);
  const secret = `pi_3Return${run}_secret_c0ffee`;
  const back = await page.goto(
    `${FRESH_HTTPS}/o/${id}?payment_intent=pi_3Return${run}&payment_intent_client_secret=${secret}&redirect_status=succeeded`,
    { referer: "https://hooks.stripe.com/" },
  );
  expect(back?.url()).toBe(`${FRESH_HTTPS}/o/${id}?returned=1`);
  await expect(page.getByTestId("payment-status")).toBeVisible();
  await page.locator("header a").first().click();
  await page.waitForURL(`${FRESH_HTTPS}/`);
  const traces = await historyTraces(page);
  expect(traces.entries.length, "the Navigation API lists the tab's entries").toBeGreaterThan(1);
  for (const seen of [...traces.entries, traces.referrer]) {
    expect(seen).not.toContain(secret);
    expect(seen).not.toContain("payment_intent");
  }
  expect(traces.referrer, "strict-origin: only the origin").not.toContain(id);
  await context.close();
});

test("same-origin bank transfer with a Packeta pickup point: instructions on the order page, no CSP violation", async ({
  browser,
}) => {
  const context = await securityContext(browser);
  const page = await context.newPage();
  const violations: string[] = [];
  page.on("console", (m) => {
    if (/Content Security Policy/i.test(m.text())) violations.push(m.text());
  });
  await sameOriginReview(page, FRESH_HTTPS, `so-bank-${run}@example.test`);
  await choosePickupPoint(page, /Trafika Vinohrady/);
  const bank = page.getByRole("radio", { name: /Bankovní převod/ });
  await expect(bank).toBeEnabled();
  await Promise.all([
    page.waitForResponse(
      (r) => r.request().method() === "PUT" && new URL(r.url()).pathname === "/_p/checkout/payment",
    ),
    bank.check(),
  ]);
  await placed(page);
  const instructions = page.getByTestId("bank-transfer");
  await expect(instructions).toBeVisible();
  await expect(instructions.locator("svg")).toHaveCount(1);
  await expect(page.getByTestId("pickup-point")).toContainText(/Trafika Vinohrady/);
  expect(violations).toEqual([]);
  await context.close();
});
