import { t } from "@platform/storefront-sdk/format";
import type { ConsentPurposes } from "@platform/storefront-sdk/types";
import { createSignal, For, Show } from "solid-js";
import { call } from "../lib/client";
import { Button, Checkbox } from "../ui.tsx";
import HydratedControls from "./HydratedControls";
import { problemText } from "./SignIn";

type Purpose = keyof ConsentPurposes;

export default function ConsentForm(props: {
  m: Record<string, string>;
  purposes: ConsentPurposes;
  textVersion: string;
  shown: Purpose[];
  endpoint: string;
}) {
  const m = props.m;
  const shown = props.shown;
  const [values, setValues] = createSignal<Record<string, boolean>>(
    Object.fromEntries(shown.map((p) => [p, props.purposes[p] === true])),
  );
  const [busy, setBusy] = createSignal(false);
  const [status, setStatus] = createSignal("");
  const [error, setError] = createSignal("");

  async function save(e: SubmitEvent) {
    e.preventDefault();
    setBusy(true);
    setStatus("");
    setError("");
    const r = await call("POST", props.endpoint, {
      purposes: values(),
      text_version: props.textVersion,
    });
    setBusy(false);
    if (r.ok) setStatus(t(m, "consent.saved"));
    else setError(problemText(m, r.code));
  }

  return (
    <form onSubmit={save}>
      <HydratedControls class="grid gap-4">
        <fieldset class="grid gap-3">
          <legend class="sr-only">{t(m, "consent.preferences_title")}</legend>
          <For each={shown}>
            {(p) => (
              <Checkbox
                class="rounded-md border border-border bg-card p-3"
                label={t(m, `consent.${p}`)}
                description={t(m, `consent.${p}_hint`)}
                checked={values()[p] ?? false}
                onChange={(v) => setValues({ ...values(), [p]: v })}
              />
            )}
          </For>
        </fieldset>
        <div>
          <Button type="submit" variant="primary" loading={busy()}>
            {t(m, "consent.save")}
          </Button>
        </div>
        <div aria-live="polite">
          <Show when={status()}>
            <p role="status" class="text-sm">
              {status()}
            </p>
          </Show>
          <Show when={error()}>
            <p role="alert" class="text-sm text-sale">
              {error()}
            </p>
          </Show>
        </div>
      </HydratedControls>
    </form>
  );
}
