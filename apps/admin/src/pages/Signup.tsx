import { Alert, Button, linkClass, TextField } from "@platform/ui";
import { A } from "@solidjs/router";
import { createSignal, Show } from "solid-js";
import { errorMessage, t } from "../i18n/index.ts";
import { requestSignup } from "../lib/signup.ts";
import { AuthLayout } from "./Login.tsx";

export function Signup() {
  const [email, setEmail] = createSignal("");
  const [sent, setSent] = createSignal(false);
  const [error, setError] = createSignal<string>();
  const [pending, setPending] = createSignal(false);

  const submit = async (e: SubmitEvent) => {
    e.preventDefault();
    setPending(true);
    setError(undefined);
    try {
      await requestSignup(email().trim());
      setSent(true);
    } catch (err) {
      setError(errorMessage(err));
    } finally {
      setPending(false);
    }
  };

  return (
    <AuthLayout
      title={sent() ? t("signup.sentTitle") : t("signup.title")}
      lead={sent() ? undefined : t("signup.lead")}
    >
      <Show
        when={!sent()}
        fallback={
          <Alert live tone="success">
            {t("signup.sentDesc", { email: email().trim() })}
          </Alert>
        }
      >
        <form class="flex flex-col gap-4" onSubmit={submit}>
          <TextField
            label={t("auth.email")}
            type="email"
            name="email"
            autocomplete="email"
            value={email()}
            onChange={setEmail}
            maxLength={254}
            required
          />
          <Show when={error()}>
            <Alert tone="error">{error()}</Alert>
          </Show>
          <Button type="submit" variant="confirm" block loading={pending()}>
            {t("signup.submit")}
          </Button>
        </form>
      </Show>
      <A href="/login" class={`self-center text-sm ${linkClass}`}>
        {t("signup.haveAccount")}
      </A>
    </AuthLayout>
  );
}
