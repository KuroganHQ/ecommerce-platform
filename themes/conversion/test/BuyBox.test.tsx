import type { ProductOption, Variant } from "@platform/storefront-sdk/types";
import { renderToString } from "solid-js/web";
import { expect, test } from "vitest";
import BuyBox from "../src/islands/BuyBox";

const options: ProductOption[] = [
  {
    code: "color",
    name: "Barva",
    values: [
      { code: "cihla", name: "Cihlová", swatch: "#b85c38" },
      { code: "popel", name: "Popelavá", swatch: "#8d9094" },
    ],
  },
  {
    code: "size",
    name: "Velikost",
    values: [
      { code: "m", name: "M", swatch: null },
      { code: "l", name: "L" },
    ],
  },
];

const variant = (sku: string, color: string, size: string, stock = "in_stock") =>
  ({
    id: sku,
    sku,
    options: { color, size },
    price: { amount: 49900, currency: "CZK", formatted: "499 Kč" },
    stock,
    image_index: null,
    is_default: sku === "CIHLA-M",
  }) as Variant;

const render = (variants: Variant[]) =>
  renderToString(() => <BuyBox options={options} variants={variants} labels={{}} />);

const labels = (html: string) => [...html.matchAll(/<label\b[^>]*>.*?<\/label>/g)].map((m) => m[0]);
const textOf = (html: string) => html.replace(/<!--.*?-->|<[^>]+>/g, "");

test("draws a colour value with a swatch as a named round radio", () => {
  const [cihla, popel] = labels(
    render([variant("CIHLA-M", "cihla", "m"), variant("POPEL-M", "popel", "m")]),
  );

  expect(cihla).toMatch(/<circle\b[^>]*fill="#b85c38"/);
  expect(cihla).toMatch(/<input\b[^>]*type="radio"[^>]*value="cihla"[^>]*checked/);
  expect(cihla).toMatch(/<span\b[^>]*class="sr-only"[^>]*>Cihlová<\/span>/);
  expect(textOf(cihla ?? "")).toBe("Cihlová");
  expect(popel).toMatch(/<circle\b[^>]*fill="#8d9094"/);
  expect(popel).not.toMatch(/<input\b[^>]*\schecked/);
});

test("keeps a value without a swatch as a text pill", () => {
  const size = labels(
    render([variant("CIHLA-M", "cihla", "m"), variant("CIHLA-L", "cihla", "l")]),
  ).slice(2);

  expect(size.map(textOf)).toEqual(["M", "L"]);
  expect(size.join("")).not.toContain("<circle");
});

test("names an unavailable swatch for screen readers", () => {
  const html = render([
    variant("CIHLA-M", "cihla", "m"),
    variant("POPEL-M", "popel", "m", "out_of_stock"),
  ]);

  expect(textOf(labels(html)[1] ?? "")).toBe("Popelavá (product.unavailable)");
});
