import { expect, test } from "vitest";
import { afterNewAttempt, returnedFromProvider } from "./payment-return";

test.each([
  ["?redirect_status=succeeded", true],
  ["?returned=1", true],
  ["?utm_source=x&returned=1", true],
  ["?redirect_status=failed", false],
  ["", false],
])("a page at %s came back from the provider: %s", (search, returned) => {
  expect(returnedFromProvider(search)).toBe(returned);
});

test.each([
  "https://shop.example/o/0b2a3f1e-6c4d-4e8a-9b1c-2d3e4f5a6b7c?returned=1",
  "https://shop.example/o/0b2a3f1e-6c4d-4e8a-9b1c-2d3e4f5a6b7c?utm_source=x&returned=1",
  `https://checkout.shop.example/o/${"a".repeat(64)}?payment_intent=pi_1&redirect_status=succeeded`,
])("a new attempt opens %s without the earlier attempt's return marker", (href) => {
  const next = afterNewAttempt(href);
  expect(returnedFromProvider(new URL(next).search), next).toBe(false);
  expect(new URL(next).pathname).toBe(new URL(href).pathname);
});
