import { createHash, randomBytes } from "node:crypto";
import { expect, test } from "@playwright/test";
import { run, sql } from "../admin/support";
import {
  checkoutOf,
  FRESH_HTTPS,
  historyTraces,
  mail,
  sameOriginOrder,
  securityContext,
} from "./support";

const OLD = checkoutOf(FRESH_HTTPS);

test("old checkout-host order, review and newsletter links reach clean shop URLs", async ({
  browser,
}) => {
  const email = `legacy-${run}@example.test`;
  const buyer = await securityContext(browser);
  const page = await buyer.newPage();
  const id = await sameOriginOrder(page, email);

  const confirmation = await mail(email, "Potvrzení objednávky");
  const token = /\/o\/([0-9a-f]{64})/.exec(confirmation.Text)?.[1] ?? "";
  expect(confirmation.Text, "a new order mail links the shop origin").toContain(
    `${FRESH_HTTPS}/o/${token}`,
  );
  expect(confirmation.Text).not.toContain("checkout.");

  const reader = await (await securityContext(browser)).newPage();
  const hops: string[] = [];
  reader.on("response", (r) => {
    if (r.status() === 308) hops.push(`${r.url()} -> ${r.headers().location}`);
  });
  await reader.goto(`${OLD}/o/${token}`);
  await reader.waitForURL(`${FRESH_HTTPS}/o/${id}`);
  expect(hops).toEqual([`${OLD}/o/${token} -> ${FRESH_HTTPS}/o/${token}`]);
  await expect(reader.getByText(email).first()).toBeVisible();
  const traces = await historyTraces(reader);
  for (const seen of [...traces.entries, traces.referrer, traces.html])
    expect(seen, "the old link is in no history entry, referrer or HTML").not.toContain(token);

  sql(`UPDATE orders SET status = 'delivered' WHERE id = '${id}'`);
  const review = randomBytes(32).toString("hex");
  const hash = createHash("sha256").update(review).digest("hex");
  sql(`INSERT INTO review_tokens (tenant_id, order_line_id, order_id, product_id, token_hash, expires_at)
    SELECT tenant_id, id, order_id, product_id, '\\x${hash}'::bytea, now() + interval '90 days'
    FROM order_lines WHERE order_id = '${id}' ORDER BY position LIMIT 1`);
  await reader.goto(`${OLD}/review?token=${review}`);
  await reader.waitForURL(`${FRESH_HTTPS}/review`);
  await expect(reader.getByRole("heading", { name: "Napsat recenzi" })).toBeVisible();
  await expect(reader.locator('input[name="_pt"]')).toHaveCount(1);

  const subscriber = `legacy-nl-${run}@example.test`;
  await page.goto(`${FRESH_HTTPS}/`);
  const signup = page.locator("form#newsletter");
  await signup.getByLabel("E-mail").fill(subscriber);
  await signup.getByRole("button", { name: "Odebírat" }).click();
  await expect(page.getByText("Hotovo! Potvrďte prosím odběr v e-mailu.")).toBeVisible();
  const confirm = await mail(subscriber, "Potvrďte odběr novinek");
  const nl = /\/newsletter\/confirm\?token=([0-9a-f]{64})/.exec(confirm.Text)?.[1] ?? "";
  expect(confirm.Text, "the confirmation links the shop origin").toContain(
    `${FRESH_HTTPS}/newsletter/confirm?token=${nl}`,
  );
  await reader.goto(`${OLD}/newsletter/confirm?token=${nl}`);
  await reader.waitForURL(`${FRESH_HTTPS}/newsletter/confirm`);
  await expect(reader.getByRole("button", { name: "Potvrdit odběr" })).toBeVisible();

  for (const path of ["//attacker.example/o", `/start?h=${"0".repeat(64)}`, "/nonsense"]) {
    const res = await reader.request.get(`${OLD}${path}`, { maxRedirects: 0 });
    expect(res.status(), path).toBe(404);
    expect(res.headers().location, path).toBeUndefined();
  }
});
