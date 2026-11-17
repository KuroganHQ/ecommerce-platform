import { randomBytes } from "node:crypto";
import { type Browser, type BrowserContext, expect, type Page, test } from "@playwright/test";
import { run, sql } from "../admin/support";
import {
  ACCOUNT_HTTPS,
  acceptAndPlace,
  CLEAN_ORDER,
  CZ_HTTPS,
  checkoutReady,
  chooseCashOnDelivery,
  cookieValue,
  detailsForm,
  FRESH_HTTPS,
  JANA,
  pageTokenMeta,
  RESTARTED,
  restartNotice,
  saveAddresses,
  saveContact,
  securityContext,
  shopTab,
  signInByEmail,
  startSameOriginCheckout,
} from "./support";

const CART = "__Host-Http-cart";
const IDT = "__Host-Http-idt";
const SESSION = "__Host-sid";
const IDENTIFY = "Máte účet? Přihlaste se";
const PLANTING =
  "only WebKit 26.6 accepts a script write of a __Host-Http- cookie; Chromium and Firefox refuse it";

const NAVIGATE = {
  "sec-fetch-site": "none",
  "sec-fetch-mode": "navigate",
  "sec-fetch-dest": "document",
  "sec-fetch-user": "?1",
};

const marker = () => `pii${randomBytes(6).toString("hex")}`;
const emailOf = (who: string, browserName: string) =>
  `${who}-${browserName}-${run}-${randomBytes(3).toString("hex")}@example.test`;
const startsWith = (prefix: string) => (url: URL) => url.href.startsWith(prefix);
const stateHash = (state: string) => `sha256(convert_to('${state}', 'UTF8'))`;

/** Orders and saved addresses of the account `email`. */
const footprint = (email: string) =>
  sql(`SELECT (SELECT count(*) FROM orders o JOIN customers c ON c.id = o.customer_id
                WHERE lower(c.email) = lower('${email}'))
         || ',' || (SELECT count(*) FROM customer_addresses a JOIN customers c ON c.id = a.customer_id
                WHERE lower(c.email) = lower('${email}'))`);

interface Shopper {
  context: BrowserContext;
  page: Page;
  email: string;
}

async function signedIn(browser: Browser, email: string): Promise<Shopper> {
  const context = await securityContext(browser);
  const page = await shopTab(context, `${ACCOUNT_HTTPS}/account`);
  await signInByEmail(page, email);
  await page.waitForURL(`${ACCOUNT_HTTPS}/account`);
  return { context, page, email };
}

/** From the address step: identify, signing in on the account origin first when `signIn` is given. */
async function identify(page: Page, signIn?: string) {
  await page.getByRole("button", { name: IDENTIFY }).click();
  if (signIn) {
    await page.waitForURL((u) => u.origin === ACCOUNT_HTTPS && u.pathname === "/account");
    await signInByEmail(page, signIn);
  }
  await page.waitForURL(`${FRESH_HTTPS}/checkout`);
}

/** An identify started while signed out stops on the account sign-in page, its state unconsumed. */
async function pendingIdentify(page: Page): Promise<string> {
  await page.getByRole("button", { name: IDENTIFY }).click();
  await page.waitForURL((u) => u.origin === ACCOUNT_HTTPS && u.pathname === "/account");
  const next = new URL(page.url()).searchParams.get("next") ?? "";
  const state = new URLSearchParams(next.split("?")[1]).get("state") ?? "";
  expect(state, "identify start minted a state").not.toBe("");
  expect(await cookieValue(page.context(), IDT), "and set it as the identify cookie").toBe(state);
  return state;
}

function plant(theme: Page, cookies: Record<string, string>): Promise<void> {
  return theme.evaluate((cookies) => {
    for (const [name, value] of Object.entries(cookies))
      document.cookie = `${name}=${value}; Path=/; Secure; SameSite=Lax`;
  }, cookies);
}

function committedUrls(page: Page): string[] {
  const seen: string[] = [];
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) seen.push(frame.url());
  });
  return seen;
}

async function expectCapabilityReadsNothing(page: Page, pii: string) {
  const html = (await (await page.goto(`${FRESH_HTTPS}/checkout`))?.text()) ?? "";
  await expect(restartNotice(page)).toHaveText(RESTARTED);
  expect(html, "the old capability's platform HTML").not.toContain(pii);
}

test("a script cannot plant the checkout capability or the identify state", async ({
  browser,
  browserName,
}) => {
  test.skip(browserName === "webkit", PLANTING);
  const attackerContext = await securityContext(browser);
  const attacker = await attackerContext.newPage();
  await startSameOriginCheckout(attacker, FRESH_HTTPS);
  await saveContact(attacker, emailOf("attacker", browserName));
  const state = await pendingIdentify(attacker);
  const cap = (await cookieValue(attackerContext, CART)) ?? "";

  const context = await securityContext(browser);
  const theme = await context.newPage();
  await theme.goto(`${FRESH_HTTPS}/`);
  await plant(theme, { [CART]: cap, [IDT]: state });
  expect(await cookieValue(context, CART), "the jar refused the capability").toBeUndefined();
  expect(await cookieValue(context, IDT), "and the identify state").toBeUndefined();
  await context.close();
  await attackerContext.close();
});

test("a capability and identify state planted by a signed-in attacker leave the attacker's account empty", async ({
  browser,
  browserName,
}) => {
  test.skip(browserName !== "webkit", PLANTING);
  const attackerContext = await securityContext(browser);
  const attacker = {
    context: attackerContext,
    page: await attackerContext.newPage(),
    email: emailOf("attacker", browserName),
  };
  await startSameOriginCheckout(attacker.page, FRESH_HTTPS);
  await saveContact(attacker.page, attacker.email);
  await identify(attacker.page, attacker.email);
  await attacker.context.clearCookies({ domain: new URL(ACCOUNT_HTTPS).hostname });
  const state = await pendingIdentify(attacker.page);
  const cap = (await cookieValue(attacker.context, CART)) ?? "";

  const victimEmail = emailOf("victim", browserName);
  const context = await securityContext(browser);
  const page = await context.newPage();
  await page.goto(`${FRESH_HTTPS}/`);
  await plant(page, { [CART]: cap, [IDT]: state });
  expect(await cookieValue(context, CART), "the theme planted the capability").toBe(cap);
  expect(await cookieValue(context, IDT), "and the identify state").toBe(state);
  await page.goto(`${FRESH_HTTPS}/checkout`);
  await expect(pageTokenMeta(page), "the planted checkout opens").toHaveCount(1);

  await identify(page, victimEmail);
  expect(await cookieValue(context, CART), "identify replaced the planted capability").not.toBe(
    cap,
  );
  expect(await cookieValue(context, IDT), "and the planted state").not.toBe(state);
  const pii = marker();
  await saveAddresses(page, { ...JANA, name: `${pii} Jana`, street: `${pii} 12` });
  await checkoutReady(page);
  await chooseCashOnDelivery(page, /PPL – kurýr/, /Testovací platba/);
  await acceptAndPlace(page);
  await page.waitForURL(CLEAN_ORDER);
  const orderId = CLEAN_ORDER.exec(page.url())?.[1] ?? "";
  expect(
    sql(
      `SELECT c.email FROM orders o JOIN customers c ON c.id = o.customer_id WHERE o.id = '${orderId}'`,
    ),
    "the order belongs to the account the victim signed into",
  ).toBe(victimEmail);

  await signInByEmail(attacker.page, attacker.email);
  await attacker.page.waitForURL(`${FRESH_HTTPS}/checkout?error=identify_failed`);
  await expectCapabilityReadsNothing(attacker.page, pii);
  expect(footprint(attacker.email), "the attacker's account has no order and no address").toBe(
    "0,0",
  );
  expect(
    sql(`SELECT count(*) FROM checkout_identities WHERE state_hash = ${stateHash(state)}
           AND consumed_at IS NOT NULL`),
    "the planted state links no account",
  ).toBe("0");
  await context.close();
  await attacker.context.close();
});

test("an identify callback this checkout did not start is refused and links nothing", async ({
  browser,
  browserName,
}) => {
  const attackerEmail = emailOf("attacker", browserName);
  const attacker = await securityContext(browser);
  const attackerPage = await attacker.newPage();
  await startSameOriginCheckout(attackerPage, FRESH_HTTPS);
  await saveContact(attackerPage, attackerEmail);
  const state = await pendingIdentify(attackerPage);
  const tab = await shopTab(attacker, `${ACCOUNT_HTTPS}/account`);
  await signInByEmail(tab, attackerEmail);
  await tab.waitForURL(`${ACCOUNT_HTTPS}/account`);
  const issued = await attacker.request.get(
    `${ACCOUNT_HTTPS}/identify?state=${state}&origin=${FRESH_HTTPS}`,
    { maxRedirects: 0, headers: NAVIGATE },
  );
  expect(issued.status(), "the account origin issued the attacker a code").toBe(303);
  const url = new URL(issued.headers().location ?? "");
  expect(url.origin + url.pathname).toBe(`${FRESH_HTTPS}/checkout/identify`);
  expect([...url.searchParams.keys()].sort()).toEqual(["code", "state"]);

  const context = await securityContext(browser);
  const page = await context.newPage();
  await startSameOriginCheckout(page, FRESH_HTTPS);
  await saveContact(page, emailOf("victim", browserName));
  const cap = await cookieValue(context, CART);
  for (const variant of [url.href, `${url.href}&next=%2F`, `${url.href}&state=x`]) {
    await page.goto(variant);
    await page.waitForURL(`${FRESH_HTTPS}/checkout?error=identify_failed`);
  }
  expect(await cookieValue(context, CART), "the victim's capability did not rotate").toBe(cap);
  expect(
    sql(`SELECT count(*) FROM checkout_identities i JOIN carts c ON c.id = i.cart_id
           WHERE c.checkout_token_hash = sha256(convert_to('${cap}', 'UTF8'))
             AND i.consumed_at IS NOT NULL`),
    "the victim's checkout is linked to no account",
  ).toBe("0");
  await expect(detailsForm(page, "addresses")).toBeVisible();
  await context.close();
  await attacker.close();
});

for (const signedInto of ["the expected account", "another account", "no account"] as const) {
  test(`placing while signed into ${signedInto}`, async ({ browser, browserName }) => {
    const account = emailOf("account", browserName);
    const contact = emailOf("contact", browserName);
    const context = await securityContext(browser);
    const page = await context.newPage();
    await startSameOriginCheckout(page, FRESH_HTTPS);
    await saveContact(page, contact);
    await identify(page, account);
    await saveAddresses(page);
    await checkoutReady(page);
    await chooseCashOnDelivery(page, /PPL – kurýr/, /Testovací platba/);

    const other = emailOf("other", browserName);
    if (signedInto !== "the expected account")
      await context.clearCookies({ domain: new URL(ACCOUNT_HTTPS).hostname });
    if (signedInto === "another account") {
      await page.goto(`${ACCOUNT_HTTPS}/account`);
      await signInByEmail(page, other);
      await page.waitForURL(`${ACCOUNT_HTTPS}/account`);
      await page.goto(`${FRESH_HTTPS}/checkout`);
      await checkoutReady(page);
    }
    await acceptAndPlace(page);

    if (signedInto === "the expected account") {
      await page.waitForURL(CLEAN_ORDER);
      const orderId = CLEAN_ORDER.exec(page.url())?.[1] ?? "";
      expect(
        sql(`SELECT c.email FROM orders o JOIN customers c ON c.id = o.customer_id
               WHERE o.id = '${orderId}'`),
      ).toBe(account);
    } else {
      await page.waitForURL(`${FRESH_HTTPS}/checkout`);
      await expect(page.getByTestId("placed-as-guest")).toHaveText(
        "Objednáno jako host: objednávka se neuloží k žádnému účtu.",
      );
      expect(
        sql(`SELECT count(*) FROM orders WHERE lower(email) = lower('${contact}')
               AND customer_id IS NULL`),
        "one guest order",
      ).toBe("1");
      expect(footprint(account).split(",")[0], "the identified account got no order").toBe("0");
      if (signedInto === "another account")
        expect(footprint(other), "nor did the other account").toBe("0,0");
    }
    await context.close();
  });
}

test("a capability rotated after the place POST aborts the placement without an order", async ({
  browser,
  browserName,
}) => {
  const account = emailOf("account", browserName);
  const contact = emailOf("contact", browserName);
  const context = await securityContext(browser);
  const page = await context.newPage();
  await startSameOriginCheckout(page, FRESH_HTTPS);
  await saveContact(page, contact);
  await identify(page, account);
  await saveAddresses(page);
  await checkoutReady(page);
  await chooseCashOnDelivery(page, /PPL – kurýr/, /Testovací platba/);

  const placement = new Promise<string>((resolve) => {
    void page.route(startsWith(`${FRESH_HTTPS}/checkout/place-order`), async (route) => {
      const res = await route.fetch({
        maxRedirects: 0,
        headers: {
          ...route.request().headers(),
          origin: FRESH_HTTPS,
          ...NAVIGATE,
          "sec-fetch-site": "same-origin",
        },
      });
      resolve(res.headers().location ?? "");
      await route.abort();
    });
  });
  await acceptAndPlace(page);
  const confirm = await placement;
  expect(
    confirm.startsWith(`${ACCOUNT_HTTPS}/placement?state=`),
    "the place POST sends the browser to the account origin",
  ).toBe(true);

  const tab = await shopTab(context, `${FRESH_HTTPS}/checkout`);
  await tab.getByRole("link", { name: "Upravit Kontakt" }).click();
  await saveContact(tab, emailOf("changed", browserName));
  await tab.close();

  await page.goto(confirm);
  await page.waitForURL(`${FRESH_HTTPS}/checkout?error=placement_failed`);
  expect(
    sql(
      `SELECT count(*) FROM orders WHERE lower(email) IN (lower('${contact}'), lower('${account}'))`,
    ),
    "no order",
  ).toBe("0");
  expect(footprint(account)).toBe("0,0");
  await context.close();
});

test("the account origin and the checkout share no session", async ({ browser, browserName }) => {
  const account = emailOf("buyer", browserName);
  const context = await securityContext(browser);
  const page = await context.newPage();
  await startSameOriginCheckout(page, FRESH_HTTPS);
  await saveContact(page, account);
  await identify(page, account);

  const names = async (origin: string) => (await context.cookies(origin)).map((c) => c.name);
  expect(await names(ACCOUNT_HTTPS)).toContain(SESSION);
  expect(await names(ACCOUNT_HTTPS), "no checkout credential on the account origin").not.toEqual(
    expect.arrayContaining([expect.stringMatching(/^__Host-Http-/)]),
  );
  expect(await names(FRESH_HTTPS), "no account session on the shop").not.toContain(SESSION);
  expect(await names(FRESH_HTTPS)).toContain(CART);

  const theme = await shopTab(context, `${FRESH_HTTPS}/`);
  const fromShop = await theme.evaluate(async (account) => {
    const own = await fetch("/_p/account/me").then((r) => r.status);
    const cross = await fetch(`${account}/_p/account/me`, { credentials: "include" }).then(
      (r) => `read ${r.status}`,
      () => "refused",
    );
    return { own, cross };
  }, ACCOUNT_HTTPS);
  expect(fromShop, "theme code reaches no account data").toEqual({ own: 404, cross: "refused" });

  const accountPage = await shopTab(context, `${ACCOUNT_HTTPS}/account`);
  const fromAccount = await accountPage.evaluate(async (shop) => {
    const own = await fetch("/_p/checkout").then((r) => r.status);
    const cross = await fetch(`${shop}/_p/checkout`, { credentials: "include" }).then(
      (r) => `read ${r.status}`,
      () => "refused",
    );
    return { own, cross };
  }, FRESH_HTTPS);
  expect(fromAccount, "account pages reach no checkout").toEqual({ own: 404, cross: "refused" });
  await context.close();
});

test("after identify, Back and the account history neither expose a code nor link again", async ({
  browser,
  browserName,
}) => {
  const account = emailOf("buyer", browserName);
  const context = await securityContext(browser);
  const page = await context.newPage();
  await startSameOriginCheckout(page, FRESH_HTTPS);
  await saveContact(page, account);
  const seen = committedUrls(page);
  await identify(page, account);
  const cap = await cookieValue(context, CART);

  for (const u of seen) expect(new URL(u).searchParams.has("code"), u).toBe(false);
  const signInPage = seen.find((u) => u.startsWith(`${ACCOUNT_HTTPS}/account?next=`)) ?? "";
  const next = new URL(signInPage).searchParams.get("next") ?? "";
  expect(next, "the sign-in page carries only the identify state").toMatch(
    /^\/identify\?state=[^&]+&origin=/,
  );

  const callbacks: string[] = [];
  page.on("request", (r) => {
    if (r.url().startsWith(`${FRESH_HTTPS}/checkout/identify`)) callbacks.push(r.url());
  });
  while (!page.url().startsWith(`${ACCOUNT_HTTPS}/account?next=`)) await page.goBack();
  await expect(page.getByRole("heading", { level: 1 })).toBeVisible();
  expect(callbacks, "going Back reaches no identify callback").toEqual([]);

  await page.goto(`${ACCOUNT_HTTPS}${next}`);
  await page.waitForURL(`${FRESH_HTTPS}/checkout?error=identify_failed`);
  expect(await cookieValue(context, CART), "replaying the state rotated nothing").toBe(cap);
  const state = new URLSearchParams(next.split("?")[1]).get("state") ?? "";
  expect(
    sql(`SELECT count(*) FROM checkout_identities WHERE state_hash = ${stateHash(state)}
           AND consumed_at IS NOT NULL`),
    "the state was consumed once",
  ).toBe("1");
  await page.goto(`${FRESH_HTTPS}/checkout`);
  await expect(pageTokenMeta(page), "the identified checkout stays open").toHaveCount(1);
  await context.close();
});

for (const [mode, shop] of [
  ["same-origin", FRESH_HTTPS],
  ["isolated", CZ_HTTPS],
] as const) {
  test(`an untrusted theme cannot change the signed-in customer's email preferences (${mode} shop)`, async ({
    browser,
    browserName,
  }) => {
    const customer = await signedIn(browser, emailOf("consent", browserName));
    const { page } = customer;
    await page.goto(`${ACCOUNT_HTTPS}/account/consent`);
    await page.getByText("E-mailový marketing", { exact: true }).click();
    await page.getByRole("button", { name: "Uložit výběr" }).click();
    await expect(page.getByRole("status")).toHaveText("Uloženo.");
    await expect(page.getByRole("checkbox", { name: "E-mailový marketing" })).toBeChecked();

    await page.goto(`${shop}/p/tricko-basic`);
    const statuses = await page.evaluate(async () => {
      const post = (body: unknown) =>
        fetch("/_p/consent", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        }).then((r) => r.status);
      return [
        await post({
          purposes: { email_marketing: false },
          text_version: "2026-10-16",
          source: "preferences",
        }),
        await post({
          purposes: { review_invites: true },
          text_version: "2026-10-16",
          source: "linked",
        }),
        await post({ purposes: { ads: false }, text_version: "2026-10-16", source: "account" }),
      ];
    });
    expect(statuses).toEqual([422, 422, 200]);
    const consent = (await customer.context.cookies(shop)).filter((c) => c.name === "consent");
    expect(
      consent.map((c) => c.value).join(),
      "the shop's cookie holds no email purpose",
    ).not.toMatch(/email|review/);

    await page.goto(`${ACCOUNT_HTTPS}/account/consent`);
    await expect(page.getByRole("checkbox", { name: "E-mailový marketing" })).toBeChecked();
    await customer.context.close();
  });
}
