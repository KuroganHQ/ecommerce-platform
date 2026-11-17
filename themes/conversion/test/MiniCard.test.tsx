import type { ProductCard } from "@platform/storefront-sdk/types";
import { renderToString } from "solid-js/web";
import { expect, test } from "vitest";
import MiniCard from "../src/lib/MiniCard";
import Swatches from "../src/lib/Swatches";

const product = {
  id: "00000000-0000-0000-0000-000000000001",
  slug: "tricko",
  name: "Tričko",
  images: [],
  price: { amount: 49900, currency: "CZK", formatted: "499 Kč" },
  price_varies: false,
  stock: "in_stock",
  badges: [],
  swatches: [
    { name: "Námořnická", color: "#1f2540" },
    { name: "Lahvová", color: "#2f4f3c" },
    { name: "Písková", color: "#d9b98a" },
    { name: "Přírodní", color: "#e4d5b8" },
    { name: "Růžová", color: "#efc6cf" },
  ],
} as unknown as ProductCard;

const colors = { "product.colors": "Barvy: {names}" };
const text = (html: string) => html.replace(/<!--.*?-->|<[^>]+>/g, "");
const forYouCard = (p: object) =>
  renderToString(() => (
    <MiniCard product={p as ProductCard} base="" labels={colors}>
      <Swatches swatches={(p as ProductCard).swatches} labels={colors} />
    </MiniCard>
  ));

test("names every colour for screen readers and shows four dots plus the rest as a count", () => {
  const html = forYouCard(product);
  expect(text(html)).toContain("Barvy: Námořnická, Lahvová, Písková, Přírodní, Růžová");
  expect(html.match(/<circle\b[^>]*fill="#[0-9a-f]{6}"/g)).toHaveLength(4);
  expect(text(html)).toContain("+1");
});

test("shows no colour row for a product without swatches or a card without the swatch row", () => {
  expect(text(forYouCard({ ...product, swatches: [] }))).not.toContain("Barvy");
  expect(forYouCard({ ...product, swatches: undefined })).not.toContain("<circle");
  const plain = renderToString(() => <MiniCard product={product} base="" labels={colors} />);
  expect(plain).not.toContain("<circle");
});
