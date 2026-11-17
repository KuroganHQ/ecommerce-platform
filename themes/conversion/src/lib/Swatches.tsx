import { type Messages, t } from "@platform/storefront-sdk/format";
import type { ProductCard } from "@platform/storefront-sdk/types";
import { For, Show } from "solid-js";

export default function Swatches(props: { swatches?: ProductCard["swatches"]; labels: Messages }) {
  const list = () => props.swatches ?? [];
  return (
    <Show when={list().length > 0}>
      <span class="mt-1.5 flex items-center">
        <span class="sr-only">
          {t(props.labels, "product.colors", {
            names: list()
              .map((s) => s.name)
              .join(", "),
          })}
        </span>
        <span class="flex items-center gap-1.5">
          <For each={list().slice(0, 4)}>
            {(s) => (
              <svg
                aria-hidden="true"
                class="size-4 rounded-full border border-black/10 ring-1 ring-white"
                viewBox="0 0 16 16"
              >
                <circle cx="8" cy="8" r="8" fill={s.color} />
              </svg>
            )}
          </For>
          <Show when={list().length > 4}>
            <span class="text-[0.8125rem] text-muted-foreground" aria-hidden="true">
              +{list().length - 4}
            </span>
          </Show>
        </span>
      </span>
    </Show>
  );
}
