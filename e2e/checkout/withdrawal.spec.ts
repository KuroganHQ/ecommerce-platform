/**
 * Withdrawal from the contract (A19, WP12) as a guest on the checkout origin, against the
 * seeded demo shop: order number + email → emailed single-use link → items → explicit
 * confirmation → receipt on screen and by email with the full declaration; the merchant
 * receives the goods (restock, A13) and refunds (incl. the outbound shipping: everything was
 * returned) from the withdrawals queue.
 */
import { expect, type Page, test } from "@playwright/test";
import { eventually, expectAccessible, run, signInOwner, sql } from "../admin/support";
import {
  acceptAndPlace,
  CZ,
  checkoutOf,
  choosePickupPoint,
  deferHydration,
  FRESH_HTTPS,
  fakePay,
  fillContactAndAddress,
  historyTraces,
  mail,
  newPage,
  order,
  pageTokenMeta,
  sameOriginOrder,
  securityContext,
  shopTab,
  themeRequests,
  toCheckout,
} from "./support";

let admin: Page;

test.beforeAll(async ({ browser }) => {
  admin = await signInOwner(browser);
});

test.afterAll(async () => {
  await admin.context().close();
});

const nav = (p: Page, name: string) =>
  p.getByRole("navigation", { name: "Main navigation" }).getByRole("link", { name, exact: true });

async function adminAction(name: string, dialog = false) {
  await admin.getByRole("button", { name, exact: true }).first().click();
  if (dialog) {
    await admin.getByRole("dialog").getByRole("button", { name, exact: true }).click();
    await expect(admin.getByRole("dialog")).toHaveCount(0);
  }
}

test("guest withdrawal: emailed link, explicit confirmation, receipt, restock and refund", async ({
  browser,
}) => {
  const email = `wp12-withdraw-${run}@example.test`;
  const page = await newPage(browser);
  await toCheckout(page, CZ, "tricko-henley");
  await fillContactAndAddress(page, email);
  await choosePickupPoint(page, /Z-BOX Praha 1/);
  await page.getByRole("radio", { name: /Testovací platba/ }).check();
  await acceptAndPlace(page);
  const token = await fakePay(page, "Pay");
  const o = await order(page, CZ, token);

  // The merchant ships and the parcel is delivered.
  await nav(admin, "Orders").click();
  await admin.getByRole("link", { name: o.number, exact: true }).click();
  await adminAction("Create label", true);
  await adminAction("Mark shipped");
  await adminAction("Mark delivered");
  await eventually(async () => (await order(page, CZ, token)).status === "delivered");

  // Step 1: order number + email (the same neutral answer for any input).
  const guest = await newPage(browser);
  await guest.goto(`${checkoutOf(CZ)}/withdraw`);
  await expect(guest.getByRole("heading", { name: "Odstoupení od smlouvy" })).toBeVisible();
  await expectAccessible(guest, "withdrawal request");
  await guest.getByLabel("Číslo objednávky").fill(o.number);
  await guest.getByLabel("E-mail").fill(email);
  await guest.getByRole("button", { name: "Poslat potvrzovací odkaz" }).click();
  await expect(guest.getByText(/poslali jsme na e-mail potvrzovací odkaz/)).toBeVisible();

  // Step 2: the emailed link opens the order's items.
  const linkMail = await mail(email, "Potvrďte odstoupení od smlouvy");
  const link = linkMail.Text.match(/https?:\/\/\S+\/withdraw\?t=[0-9a-f]{64}/)?.[0] ?? "";
  expect(link).not.toBe("");
  const release = await deferHydration(guest);
  try {
    await guest.goto(link, { waitUntil: "commit" });
    await expect(guest.getByRole("checkbox", { name: /Henley/ })).toBeVisible();
    await expect(guest.getByRole("checkbox", { name: /Henley/ })).toBeDisabled();
  } finally {
    release();
  }
  await guest.getByRole("checkbox", { name: /Henley/ }).check();
  await expectAccessible(guest, "withdrawal form");
  await guest.getByRole("button", { name: "Pokračovat" }).click();
  // Step 3: nothing is sent before the explicit confirmation.
  expect(
    sql(
      `SELECT count(*) FROM withdrawals w JOIN orders o ON o.id = w.order_id WHERE o.number = ${Number(o.number)}`,
    ),
  ).toBe("0");
  await guest.getByRole("button", { name: "Potvrdit odstoupení" }).click();
  await expect(guest.getByRole("heading", { name: "Odstoupení přijato" })).toBeVisible();
  await expect(guest.getByText(o.number).first()).toBeVisible();
  const receipt = await mail(email, `Odstoupení od objednávky ${o.number} přijato`);
  expect(receipt.Text).toContain(o.number);
  expect(receipt.Text).toContain("Henley");

  // The link works once.
  await guest.goto(link);
  await expect(guest.getByText(/Odkaz je neplatný/)).toBeVisible();

  // Merchant: goods received (restock), then the refund with the outbound shipping.
  const restocksBefore = Number(
    sql(`SELECT count(*) FROM stock_movements WHERE kind = 'restock' AND ref_type = 'return_line'`),
  );
  await nav(admin, "Withdrawals").click();
  const row = admin.getByRole("row").filter({ hasText: o.number }).first();
  await expect(row).toBeVisible();
  await expectAccessible(admin, "withdrawals queue");
  await row.getByRole("button", { name: "Receive goods" }).click();
  await admin.getByRole("dialog").getByRole("button", { name: "Receive goods" }).click();
  await expect(admin.getByRole("dialog")).toHaveCount(0);
  await eventually(
    () =>
      Number(
        sql(
          `SELECT count(*) FROM stock_movements WHERE kind = 'restock' AND ref_type = 'return_line'`,
        ),
      ) > restocksBefore,
  );
  await row.getByRole("button", { name: "Refund withdrawal" }).click();
  await admin.getByRole("dialog").getByRole("button", { name: "Refund withdrawal" }).click();
  await eventually(async () => (await order(page, CZ, token)).status === "returned");
  // Return state commits before the refund job settles the payment; assert that transition too.
  await eventually(async () => (await order(page, CZ, token)).payment.status === "refunded");
  const refunded = await order(page, CZ, token);
  expect(refunded.payment.status).toBe("refunded");
  const shipping = sql(
    `SELECT count(*) FROM refunds r JOIN orders o ON o.id = r.order_id
     WHERE o.number = ${Number(o.number)} AND r.lines @> '[{"charge": "shipping"}]'`,
  );
  expect(shipping).toBe("1");
  await mail(email, `Vrácení peněz k objednávce ${o.number}`);
  await guest.context().close();
  await page.context().close();
});

test("same-origin withdrawal: request grant, emailed link on a clean URL, refused IBAN, supersession, single use", async ({
  browser,
}) => {
  const email = `so-withdraw-${run}@example.test`;
  const buyer = await securityContext(browser);
  const page = await buyer.newPage();
  const id = await sameOriginOrder(page, email);
  const number = sql(`SELECT number FROM orders WHERE id = '${id}'`);
  sql(`UPDATE orders SET status = 'shipped' WHERE id = '${id}'`);
  await page.reload();

  await page.getByRole("link", { name: "Odstoupit od nákupu" }).click();
  await page.waitForURL(`${FRESH_HTTPS}/withdraw`);
  await expect(pageTokenMeta(page), "the request grant's page token").toHaveCount(1);
  await page.getByLabel("Číslo objednávky").fill(number);
  await page.getByLabel("E-mail").fill(email);
  await page.getByRole("button", { name: "Poslat potvrzovací odkaz" }).click();
  await expect(page.getByText(/poslali jsme na e-mail potvrzovací odkaz/)).toBeVisible();

  const linkMail = await mail(email, "Potvrďte odstoupení od smlouvy");
  const token = /\/withdraw\?t=([0-9a-f]{64})/.exec(linkMail.Text)?.[1] ?? "";
  expect(token).toMatch(/^[0-9a-f]{64}$/);
  await page.goto(`${FRESH_HTTPS}/withdraw?t=${token}`);
  await page.waitForURL(`${FRESH_HTTPS}/withdraw/declaration`);
  const traces = await historyTraces(page);
  for (const trace of [...traces.entries, traces.referrer, traces.html])
    expect(trace, "no withdrawal link in history, referrer or HTML").not.toContain(token);
  await expectAccessible(page, "same-origin withdrawal form");

  const theme = await shopTab(buyer, `${FRESH_HTTPS}/`);
  const reads = await themeRequests(theme, [
    ["GET", "/withdraw/declaration"],
    ["POST", "/_p/withdraw/declaration"],
    ["POST", "/_p/withdraw"],
  ]);
  for (const r of reads) {
    expect(r, "theme script gets no withdrawal data").toMatch(/ 403 $/);
    expect(r).not.toContain(number);
  }

  const declare = async (iban: string) => {
    await page.getByRole("checkbox", { name: /Tričko/ }).check();
    await page.getByLabel("IBAN pro vrácení peněz").fill(iban);
    await page.getByRole("button", { name: "Pokračovat" }).click();
    await page.getByRole("button", { name: "Potvrdit odstoupení" }).click();
  };
  await declare("CZ00 nonsense");
  await expect(page.getByRole("alert")).toContainText("Zkontrolujte IBAN");
  await page.getByRole("button", { name: "Zpět k úpravám" }).click();

  const other = await buyer.newPage();
  await other.goto(`${FRESH_HTTPS}/withdraw`);
  await page.getByLabel("IBAN pro vrácení peněz").fill("CZ6508000000192000145399");
  await page.getByRole("button", { name: "Pokračovat" }).click();
  await page.getByRole("button", { name: "Potvrdit odstoupení" }).click();
  await expect(
    page.getByText(/Odkaz je neplatný/),
    "a newer grant in this browser superseded the declaration page",
  ).toBeVisible();
  expect(sql(`SELECT count(*) FROM withdrawals WHERE order_id = '${id}'`)).toBe("0");

  await page.goto(`${FRESH_HTTPS}/withdraw?t=${token}`);
  await page.waitForURL(`${FRESH_HTTPS}/withdraw/declaration`);
  await declare("CZ6508000000192000145399");
  await expect(page.getByRole("heading", { name: "Odstoupení přijato" })).toBeVisible();
  await mail(email, `Odstoupení od objednávky ${number} přijato`);

  await page.goto(`${FRESH_HTTPS}/withdraw?t=${token}`);
  await page.waitForURL(`${FRESH_HTTPS}/withdraw?link=invalid`);
  await expect(page.getByText(/Odkaz je neplatný/)).toBeVisible();
  expect(sql(`SELECT count(*) FROM withdrawals WHERE order_id = '${id}'`)).toBe("1");
  await buyer.close();
});
