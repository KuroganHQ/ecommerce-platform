/**
 * Customer account on its own origin (SHOP-95, A1, A5, A20) against the seeded demo shop
 * (`make up && make seed`): email-link sign-in via Mailpit, addresses, password, sign-out,
 * password sign-in, email preferences, old account links redirected there, and the theme's
 * consent banner → the platform record → the checkout origin's preferences page.
 */

import { expect, type Page, test } from "@playwright/test";
import { expectAccessible, mailpit, run } from "../admin/support";
import { testContext } from "../rate-client";
import { ACCOUNT_SK, ACCOUNT as account, deferHydration } from "./support";

const port = process.env.HTTP_PORT ?? "8080";
const shop = `http://demo.localhost:${port}`;
const checkout = `http://checkout.demo.localhost:${port}`;
const email = `zakaznik-${run}@example.test`;
const password = "správné koňské heslo";

test.describe.configure({ mode: "serial" });

let page: Page;

test.beforeAll(async ({ browser }) => {
  page = await (await testContext(browser, { locale: "cs-CZ" })).newPage();
});

test.afterAll(async () => {
  await page.context().close();
});

/** The newest sign-in link for `to` (the checkout's own mail, through the worker). */
async function accountLink(to: string, since: Date): Promise<string> {
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    const res = await fetch(`${mailpit}/api/v1/search?query=${encodeURIComponent(`to:"${to}"`)}`);
    const body = (await res.json()) as { messages: { ID: string; Created: string }[] };
    const fresh = body.messages
      .filter((m) => new Date(m.Created) >= since)
      .sort((a, b) => b.Created.localeCompare(a.Created))[0];
    if (fresh) {
      const msg = (await (await fetch(`${mailpit}/api/v1/message/${fresh.ID}`)).json()) as {
        Text: string;
      };
      const link = msg.Text.match(/https?:\/\/\S+\/account\/verify\?token=[0-9a-f]{64}/);
      if (link) return link[0];
    }
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error(`no sign-in link for ${to}`);
}

test("email-link sign-in on the account origin creates the account", async () => {
  await page.goto(`${account}/`);
  await expect(page).toHaveURL(`${account}/account`);
  await expect(page.getByRole("heading", { level: 1, name: "Přihlášení" })).toBeVisible();
  await expectAccessible(page, "sign-in");
  const since = new Date(Date.now() - 1000);
  const linkForm = page.locator("form").first();
  await linkForm.getByLabel("E-mail").fill(email);
  await linkForm.getByRole("button", { name: "Poslat přihlašovací odkaz" }).click();
  await expect(page.getByRole("status")).toContainText(email);

  const link = await accountLink(email, since);
  expect(link.startsWith(`${account}/account/verify?token=`)).toBe(true);
  const verify = await page.goto(link);
  expect(verify?.headers()["referrer-policy"]).toBe("no-referrer");
  await page.getByRole("button", { name: "Přihlásit se" }).click();
  await page.waitForURL(`${account}/account`);
  await expect(page.getByTestId("signed-in-as")).toContainText(email);
  await expectAccessible(page, "account");

  // The link was single use.
  await page.goto(link);
  await page.getByRole("button", { name: "Přihlásit se" }).click();
  await expect(page.getByRole("alert")).toContainText("Odkaz vypršel");

  const cookies = await page.context().cookies(account);
  const sid = cookies.find((c) => c.name === "__Host-sid");
  expect(sid?.httpOnly).toBe(true);
  expect(sid?.sameSite).toBe("Lax");
  expect(sid?.domain).toBe("demo.accounts.localhost");
  for (const origin of [shop, checkout, ACCOUNT_SK])
    expect((await page.context().cookies(origin)).some((c) => c.name === "__Host-sid")).toBe(false);

  await expect(page.getByRole("banner").getByRole("link").first()).toHaveAttribute(
    "href",
    `${shop}/`,
  );
});

test("address controls wait for hydration", async () => {
  const release = await deferHydration(page);
  const add = page.getByRole("button", { name: "Přidat adresu" });
  try {
    await page.goto(`${account}/account/addresses`, { waitUntil: "commit" });
    await expect(add).toBeVisible();
    await expect(add).toBeDisabled();
  } finally {
    release();
  }
  await add.click();
  await expect(page.getByLabel("Jméno a příjmení")).toBeVisible();
});

test("adds an address", async () => {
  await page.goto(`${account}/account`);
  await page.getByRole("link", { name: /Adresy/ }).click();
  await expect(page.getByText("Zatím nemáte uloženou žádnou adresu.")).toBeVisible();
  await page.getByRole("button", { name: "Přidat adresu" }).click();
  await page.getByLabel("Jméno a příjmení").fill("Jana Nováková");
  await page.getByLabel("Ulice a číslo domu").fill("Dlouhá 12");
  await page.getByLabel("PSČ").fill("110 00");
  await page.getByLabel("Město").fill("Praha");
  await page.getByRole("button", { name: "Uložit adresu" }).click();
  await expect(page.getByRole("status")).toHaveText("Adresa byla uložena.");
  await expect(page.locator("address")).toContainText("Dlouhá 12");
  await expect(page.locator("address")).toContainText("Výchozí");
  await expectAccessible(page, "addresses");
  await page.reload();
  await expect(page.locator("address")).toContainText("110 00 Praha");

  await page.getByRole("button", { name: /^Upravit/ }).click();
  await page.getByLabel("Telefon (nepovinné)").fill("+420 777 123 456");
  await page.getByRole("button", { name: "Uložit adresu" }).click();
  await expect(page.getByRole("status")).toHaveText("Adresa byla uložena.");
  await page.reload();
  await expect(page.locator("address")).toContainText("+420 777 123 456");
});

test("sets a password right after the email-link sign-in, then signs out", async () => {
  const releasePassword = await deferHydration(page);
  try {
    await page.goto(`${account}/account/security`, { waitUntil: "commit" });
    await expect(page.locator('input[autocomplete="new-password"]')).toBeVisible();
    await expect(page.locator('input[autocomplete="new-password"]')).toBeDisabled();
    await expect(page.getByRole("button", { name: "Uložit heslo" })).toBeDisabled();
  } finally {
    releasePassword();
  }
  await expect(page.getByLabel("Současné heslo")).toHaveCount(0);
  await page.getByLabel("Nové heslo").fill(password);
  await page.getByRole("button", { name: "Uložit heslo" }).click();
  await expect(page.getByRole("status")).toContainText("Heslo je uložené");
  await expectAccessible(page, "security");

  const releaseSignOut = await deferHydration(page);
  try {
    await page.goto(`${account}/account`, { waitUntil: "commit" });
    await expect(page.getByRole("button", { name: "Odhlásit se" })).toBeVisible();
    await expect(page.getByRole("button", { name: "Odhlásit se" })).toBeDisabled();
  } finally {
    releaseSignOut();
  }
  await page.getByRole("button", { name: "Odhlásit se" }).click();
  await expect(page.getByRole("heading", { level: 1, name: "Přihlášení" })).toBeVisible();
  // Account pages need a session again.
  await page.goto(`${account}/account/addresses`);
  await expect(page).toHaveURL(`${account}/account?next=/account/addresses`);
});

test("password sign-in on the account origin", async () => {
  await page.goto(`${account}/account?next=/account/security`);
  await page.locator("summary", { hasText: "Heslem" }).click();
  const form = page.locator("details form");
  await form.getByLabel("E-mail").fill(email);
  await form.getByLabel("Heslo").fill("wrong password!!");
  await form.getByRole("button", { name: "Přihlásit se" }).click();
  await expect(page.getByRole("alert")).toHaveText("E-mail nebo heslo nesedí.");
  await form.getByLabel("Heslo").fill(password);
  await form.getByRole("button", { name: "Přihlásit se" }).click();
  await page.waitForURL(`${account}/account/security`);
  await expect(page.getByLabel("Současné heslo")).toBeVisible();
});

test("email_preferences_save_and_reload", async () => {
  await page.goto(`${account}/account`);
  await page.getByRole("link", { name: /Soukromí a souhlasy/ }).click();
  await expect(page).toHaveURL(`${account}/account/consent`);
  await expect(page.getByRole("heading", { level: 1, name: "Soukromí a souhlasy" })).toBeVisible();
  await expect(page.getByRole("checkbox")).toHaveCount(2);
  const newsletter = page.getByRole("checkbox", { name: "E-mailový marketing" });
  await expect(newsletter).not.toBeChecked();
  await expectAccessible(page, "email preferences");
  await page.getByText("E-mailový marketing", { exact: true }).click();
  await page.getByRole("button", { name: "Uložit výběr" }).click();
  await expect(page.getByRole("status")).toHaveText("Uloženo.");
  await page.reload();
  await expect(page.getByRole("checkbox", { name: "E-mailový marketing" })).toBeChecked();
  expect((await page.context().cookies(shop)).some((c) => c.name === "consent")).toBe(false);

  await page.getByText("E-mailový marketing", { exact: true }).click();
  await page.getByRole("button", { name: "Uložit výběr" }).click();
  await expect(page.getByRole("status")).toHaveText("Uloženo.");
  await page.reload();
  await expect(page.getByRole("checkbox", { name: "E-mailový marketing" })).not.toBeChecked();
});

test("old_account_links_land_on_account_origin", async () => {
  const token = "a".repeat(64);
  for (const [from, path] of [
    [checkout, "/account"],
    [checkout, "/account/addresses"],
    [checkout, `/account/verify?token=${token}`],
    [shop, "/account/security"],
  ] as const) {
    await page.goto(`${from}${path}`);
    await expect(page).toHaveURL(`${account}${path}`);
  }
  for (const from of [checkout, shop]) {
    const res = await page.request.get(`${from}/account`, { maxRedirects: 0 });
    expect(res.status()).toBe(308);
    expect(res.headers()["cache-control"]).toBe("no-store");
  }
  for (const path of ["/_p/account/me", "/cs/account"])
    expect((await page.request.get(`${checkout}${path}`, { maxRedirects: 0 })).status()).toBe(404);
});

test("the theme's consent banner is recorded, then changed on the preferences page", async () => {
  await page.goto(`${shop}/p/tricko-basic`);
  const banner = page.getByRole("region", { name: "Souhlas s cookies" });
  await expect(banner).toBeVisible();
  await banner.getByRole("button", { name: "Přijmout vše" }).click();
  await expect(banner).toBeHidden();
  // The platform recorded it: the edge answered with the anonymous subject (HttpOnly).
  await expect
    .poll(async () =>
      (await page.context().cookies(shop)).find((c) => c.name === "__Secure-consent_id"),
    )
    .toMatchObject({ httpOnly: true, domain: ".demo.localhost" });
  const consentCookies = (await page.context().cookies(shop)).filter((c) => c.name === "consent");
  expect(consentCookies.map((c) => c.value)).toEqual(["analytics%2Cads%2Cpersonalization"]);

  // The checkout origin's preferences page shows the server's record and changes it.
  await page.goto(`${checkout}/consent`);
  await expect(page.getByRole("heading", { level: 1, name: "Nastavení souhlasů" })).toBeVisible();
  await expect(page.getByRole("checkbox", { name: "Měření návštěvnosti" })).toBeChecked();
  await expect(page.getByRole("checkbox", { name: "Reklama" })).toBeChecked();
  await expectAccessible(page, "consent");
  await page.getByText("Reklama", { exact: true }).click();
  await page.getByRole("button", { name: "Uložit výběr" }).click();
  await expect(page.getByRole("status")).toHaveText("Uloženo.");
  // One `consent` cookie for the shop, now without ads; the banner stays closed there.
  const after = (await page.context().cookies(shop)).filter((c) => c.name === "consent");
  expect(after.map((c) => c.value)).toEqual(["analytics%2Cpersonalization"]);
  await page.goto(`${shop}/p/tricko-basic`);
  await expect(page.getByRole("button", { name: "Nastavení cookies" })).toBeVisible();
  await expect(page.getByRole("region", { name: "Souhlas s cookies" })).toBeHidden();
});
