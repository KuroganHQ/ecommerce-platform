import { Alert, Button, TextField } from "@platform/ui";
import { createMutation, useQueryClient } from "@tanstack/solid-query";
import { createSignal, Show } from "solid-js";
import { errorMessage, t } from "../i18n/index.ts";
import { api, setTenantId, unwrap } from "../lib/api.ts";

/** The first store of a signed-in user without memberships (self-signup). */
export function CreateStore() {
  const qc = useQueryClient();
  const [name, setName] = createSignal("");
  const [slug, setSlug] = createSignal("");
  const [error, setError] = createSignal<string>();

  const create = createMutation(() => ({
    mutationFn: () =>
      unwrap(
        api.POST("/admin/v1/stores", {
          body: { name: name().trim(), slug: slug().trim().toLowerCase() },
        }),
      ),
    onSuccess: async (store) => {
      setTenantId(store.tenant_id);
      await qc.invalidateQueries({ queryKey: ["me"] });
    },
    onError: (err) => setError(errorMessage(err)),
  }));

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    setError(undefined);
    create.mutate();
  };

  return (
    <form class="flex flex-col gap-4" onSubmit={submit}>
      <TextField
        label={t("signup.storeName")}
        value={name()}
        onChange={setName}
        maxLength={200}
        required
      />
      <TextField
        label={t("signup.storeSlug")}
        description={t("signup.storeSlugHint")}
        value={slug()}
        onChange={setSlug}
        autocomplete="off"
        maxLength={63}
        required
      />
      <Show when={error()}>
        <Alert tone="error">{error()}</Alert>
      </Show>
      <Button type="submit" variant="confirm" block loading={create.isPending}>
        {t("signup.createStore")}
      </Button>
    </form>
  );
}
