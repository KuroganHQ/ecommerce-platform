import { expect, type Page, test } from "@playwright/test";
import { mailpit, run } from "../admin/support";
import {
  ACCOUNT,
  acceptAndPlace,
  checkoutReady,
  chooseCashOnDelivery,
  FRESH,
  saveAddresses,
  saveContact,
  securityContext,
  startSameOriginCheckout,
} from "./support";

const CLEAN_ORDER = /\/o\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

async function signInLink(to: string, since: Date): Promise<string> {
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

async function signIn(page: Page, email: string) {
  await page.goto(`${ACCOUNT}/account`);
  const since = new Date(Date.now() - 1000);
  const form = page.locator("form").first();
  await form.getByLabel("E-mail").fill(email);
  await form.getByRole("button", { name: "Poslat přihlašovací odkaz" }).click();
  await expect(page.getByRole("status")).toContainText(email);
  await page.goto(await signInLink(email, since));
  await page.getByRole("button", { name: "Přihlásit se" }).click();
  await page.waitForURL(`${ACCOUNT}/account`);
}

async function linkedReview(page: Page, email: string) {
  await startSameOriginCheckout(page, FRESH);
  await saveContact(page, email);
  await page.getByRole("button", { name: "Máte účet? Přihlaste se" }).click();
  await page.waitForURL(`${FRESH}/checkout`);
  await saveAddresses(page);
  await checkoutReady(page);
  await chooseCashOnDelivery(page, /PPL – kurýr/, /Testovací platba/);
}

function navigations(page: Page): { requested: string[]; committed: string[] } {
  const seen = { requested: [] as string[], committed: [] as string[] };
  page.on("request", (r) => {
    if (r.isNavigationRequest() && r.frame() === page.mainFrame()) seen.requested.push(r.url());
  });
  page.on("framenavigated", (frame) => {
    if (frame === page.mainFrame()) seen.committed.push(frame.url());
  });
  return seen;
}

test("a linked checkout placed with no account signed in shows the guest result before payment", async ({
  browser,
}) => {
  const account = `placement-guest-${run}@example.test`;
  const buyer = `buyer-guest-${run}@example.test`;
  const context = await securityContext(browser);
  const page = await context.newPage();
  await signIn(page, account);
  await linkedReview(page, buyer);
  await context.clearCookies({ domain: "demo.accounts.localhost" });

  const seen = navigations(page);
  await acceptAndPlace(page);
  await page.waitForURL(`${FRESH}/checkout`);
  const receipt = page.getByRole("status").filter({ hasText: "Děkujeme za objednávku!" });
  await expect(receipt.getByTestId("placed-as-guest")).toHaveText(
    "Objednáno jako host: objednávka se neuloží k žádnému účtu.",
  );
  await expect(receipt).toContainText(`Potvrzení pošleme na ${buyer}`);
  expect(
    seen.requested.some((u) => u.includes("/_p/fake-pay/")),
    "no payment step before the guest result",
  ).toBe(false);
  expect(seen.requested.some((u) => u.startsWith(`${ACCOUNT}/placement?`))).toBe(true);
  expect(seen.requested.some((u) => u.startsWith(`${FRESH}/checkout/placement?`))).toBe(true);
  for (const u of seen.committed)
    expect(new URL(u).searchParams.has("code"), `no rendered page holds the code: ${u}`).toBe(
      false,
    );

  await page.getByRole("link", { name: "Pokračovat k objednávce" }).click();
  await page.waitForURL(CLEAN_ORDER);
  await context.close();
});

test("a linked checkout placed while the same account is signed in lands on its order", async ({
  browser,
}) => {
  const account = `placement-same-${run}@example.test`;
  const context = await securityContext(browser);
  const page = await context.newPage();
  await signIn(page, account);
  await linkedReview(page, account);

  await acceptAndPlace(page);
  await page.waitForURL(CLEAN_ORDER);
  await expect(page.getByTestId("placed-as-guest")).toHaveCount(0);
  await context.close();
});
