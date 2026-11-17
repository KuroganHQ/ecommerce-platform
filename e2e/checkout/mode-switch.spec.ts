import { type APIResponse, expect, type Page, test } from "@playwright/test";
import { createTenant, magicLink, run, sql, useEnglish } from "../admin/support";
import { rateHeaders, testContext } from "../rate-client";
import { checkoutOf, pageTokenMeta, RESTARTED, restartNotice, submitStart } from "./support";

test.describe.configure({ mode: "serial" });

const port = process.env.HTTP_PORT ?? "8080";
const slug = `modes-${run}`;
const SHOP = `http://${slug}.localhost:${port}`;
const OLD = checkoutOf(SHOP);
const API = `http://api.localhost:${port}/admin/v1`;
const owner = `modes-owner-${run}@example.test`;

let tenant: string;
let token: string;
let ownerPage: Page;
let shopper: Page;
let variant: string;

function admin(method: string, path: string, data?: unknown): Promise<APIResponse> {
  return ownerPage.request.fetch(`${API}/${path}`, {
    method,
    headers: { authorization: `Bearer ${token}`, "x-tenant-id": tenant },
    ...(data === undefined ? {} : { data }),
  });
}

async function expectStatus(res: APIResponse, status: number) {
  expect(res.status(), `${res.url()}: ${await res.text()}`).toBe(status);
  return res.json();
}

async function addLine() {
  await shopper.goto(`${SHOP}/`);
  const line = await shopper.request.post(`${SHOP}/_p/cart/lines`, {
    headers: { origin: SHOP, ...rateHeaders(shopper) },
    data: { variant_id: variant, quantity: 1 },
  });
  expect(line.status()).toBe(200);
}

const generation = () =>
  sql(
    `SELECT mode || ' ' || mode_generation FROM platform.checkout_origins WHERE origin = '${SHOP}'`,
  );

test.beforeAll(async ({ browser }) => {
  tenant = createTenant(slug, `Modes ${run}`, owner).tenant_id;
  sql(`INSERT INTO platform.checkout_origins (scheme, hostname, port, tenant_id, market_id, history, mode)
         SELECT 'http', hostname, ${Number(port)}, tenant_id, market_id,
                'platform_never_reassigned', 'same_origin'
         FROM platform.domains WHERE hostname = '${slug}.localhost'`);

  ownerPage = await (await testContext(browser)).newPage();
  await useEnglish(ownerPage);
  await ownerPage.goto("/login");
  const since = new Date(Date.now() - 1000);
  await ownerPage.getByLabel("Email").fill(owner);
  await ownerPage.getByRole("button", { name: "Email me a sign-in link" }).click();
  await ownerPage.goto(await magicLink(owner, since));
  await expect(ownerPage.getByRole("heading", { name: "Overview", exact: true })).toBeVisible();
  const auth = await ownerPage.request.get(new URL("/api/auth/token", ownerPage.url()).toString(), {
    headers: rateHeaders(ownerPage),
  });
  token = ((await auth.json()) as { token: string }).token;

  await expectStatus(
    await admin("PUT", "tax-profile", {
      establishment_country: "CZ",
      vat_payer: true,
      vat_id: "CZ12345678",
      distance_sales_mode: "destination",
    }),
    200,
  );
  const product = (await expectStatus(
    await admin("POST", "products", {
      status: "active",
      translations: [{ locale: "cs", name: "Hrnek", slug: `hrnek-${run}` }],
      variants: [{ sku: `MODES-${run}` }],
    }),
    201,
  )) as { variants: { id: string }[] };
  variant = product.variants[0]?.id ?? "";
  const market = sql(`SELECT id FROM markets WHERE tenant_id = '${tenant}' AND is_default`);
  const list = (await expectStatus(
    await admin("POST", "price-lists", {
      code: "cz",
      name: "Česko",
      currency: "CZK",
      market_ids: [market],
    }),
    201,
  )) as { id: string };
  await expectStatus(
    await admin("PUT", `price-lists/${list.id}/prices`, {
      items: [{ variant_id: variant, amount_minor: 29000 }],
    }),
    200,
  );
  await expectStatus(
    await admin("POST", `inventory/${variant}/adjustments`, { on_hand: 20, note: "e2e" }),
    201,
  );

  const context = await testContext(browser, { locale: "cs-CZ" });
  await context.addCookies([{ name: "consent", value: "", url: SHOP }]);
  shopper = await context.newPage();
  await addLine();
});

test.afterAll(async () => {
  await shopper?.context().close();
  await ownerPage?.context().close();
});

test("switching a same-origin shop to isolated ends its checkout and hands checkout to the checkout host", async () => {
  await Promise.all([shopper.waitForURL(`${SHOP}/checkout`), submitStart(shopper)]);
  await expect(pageTokenMeta(shopper), "a same-origin checkout is open").toHaveCount(1);
  expect(generation()).toBe("same_origin 1");

  const switched = (await expectStatus(
    await admin("POST", "checkout-origins/mode", { origin: SHOP, mode: "isolated" }),
    200,
  )) as { mode: string; mode_generation: number };
  expect(switched).toMatchObject({ mode: "isolated", mode_generation: 2 });

  expect((await shopper.goto(`${SHOP}/checkout`))?.status(), "the shop serves no checkout").toBe(
    404,
  );
  expect(
    await shopper.evaluate(() => fetch("/_p/checkout").then((r) => r.status)),
    "nor its API",
  ).toBe(404);
  await shopper.goto(`${SHOP}/`);
  await Promise.all([shopper.waitForURL(`${OLD}/`), submitStart(shopper)]);
  await expect(shopper.getByRole("heading", { name: "Pokladna", level: 1 })).toBeVisible();
});

test("switching back to same-origin ends the isolated checkout and serves checkout on the shop", async () => {
  const res = await admin("POST", "checkout-origins/mode", { origin: SHOP, mode: "same_origin" });
  const body = (await res.json()) as { code?: string; mode?: string; mode_generation?: number };
  test.skip(
    res.status() === 409 && body.code === "checkout_not_ready",
    "this stack runs without SAME_ORIGIN_CHECKOUT_READY=1",
  );
  expect(res.status()).toBe(200);
  expect(body).toMatchObject({ mode: "same_origin", mode_generation: 3 });

  await shopper.goto(`${OLD}/`);
  await shopper.waitForURL(`${SHOP}/checkout`);
  await expect(
    restartNotice(shopper),
    "the capability from before both switches opens nothing",
  ).toHaveText(RESTARTED);
  await expect(pageTokenMeta(shopper)).toHaveCount(0);
  const handoff = await shopper.request.get(`${OLD}/start?h=${"0".repeat(64)}`, {
    maxRedirects: 0,
  });
  expect(handoff.status(), "the handoff is gone").toBe(404);

  await addLine();
  await Promise.all([shopper.waitForURL(`${SHOP}/checkout`), submitStart(shopper)]);
  await expect(pageTokenMeta(shopper), "a new start opens").toHaveCount(1);
  expect(generation()).toBe("same_origin 3");
});
