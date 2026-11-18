import { Alert, Badge, Button, Card, EmptyState, PermissionDenied, type Tone } from "@platform/ui";
import { useSearchParams } from "@solidjs/router";
import { createMutation, createQuery } from "@tanstack/solid-query";
import { createSignal, For, Show } from "solid-js";
import { PageHeader, QueryState } from "../components/Page.tsx";
import type { Dictionary } from "../i18n/en.ts";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type Status = keyof Dictionary["billing"]["statuses"];

const TONES: Partial<Record<Status, Tone>> = {
  trialing: "info",
  active: "success",
  past_due: "warning",
  unpaid: "error",
  incomplete: "warning",
};

export default function Billing() {
  const { can } = useMembership();
  const [params] = useSearchParams();
  const [error, setError] = createSignal<string>();

  const billing = createQuery(() => ({
    queryKey: tenantKey("billing"),
    queryFn: () => unwrap(api.GET("/admin/v1/billing", { params: { header: tenantHeader() } })),
    enabled: can("owner"),
  }));

  const redirect = createMutation(() => ({
    mutationFn: (plan: string | null) =>
      unwrap(
        plan === null
          ? api.POST("/admin/v1/billing/portal", { params: { header: tenantHeader() } })
          : api.POST("/admin/v1/billing/checkout", {
              params: { header: tenantHeader() },
              body: { plan },
            }),
      ),
    onSuccess: (r) => window.location.assign(r.url),
    onError: (err) => setError(errorMessage(err)),
  }));
  const go = (plan: string | null) => {
    setError(undefined);
    redirect.mutate(plan);
  };

  return (
    <Show
      when={can("owner")}
      fallback={
        <>
          <PageHeader title={t("billing.title")} />
          <PermissionDenied
            title={t("common.forbiddenTitle")}
            description={t("common.forbiddenDesc")}
          />
        </>
      }
    >
      <PageHeader title={t("billing.title")} description={t("billing.lead")} />
      <QueryState query={billing}>
        {(b) => (
          <Show
            when={b.configured}
            fallback={
              <EmptyState
                icon="settings"
                title={t("billing.notConfiguredTitle")}
                description={t("billing.notConfiguredDesc")}
              />
            }
          >
            <div class="flex max-w-2xl flex-col gap-4">
              <Show when={params.checkout === "done"}>
                <Alert live tone="success">
                  {t("billing.checkoutDone")}
                </Alert>
              </Show>
              <Show when={error()}>
                <Alert tone="error">{error()}</Alert>
              </Show>
              <Card title={t("billing.current")}>
                <dl class="grid grid-cols-1 gap-3 px-4 pb-4 text-sm sm:grid-cols-3">
                  <div class="flex flex-col gap-1">
                    <dt class="text-muted-foreground">{t("billing.plan")}</dt>
                    <dd class="font-semibold text-heading">{b.plan}</dd>
                  </div>
                  <div class="flex flex-col gap-1">
                    <dt class="text-muted-foreground">{t("billing.status")}</dt>
                    <dd>
                      <Show when={b.status} fallback={t("billing.noSubscription")}>
                        {(s) => {
                          const status = s() as Status;
                          return (
                            <Badge tone={TONES[status] ?? "neutral"}>
                              {t(`billing.statuses.${status}`)}
                            </Badge>
                          );
                        }}
                      </Show>
                    </dd>
                  </div>
                  <Show when={b.current_period_end}>
                    {(end) => (
                      <div class="flex flex-col gap-1">
                        <dt class="text-muted-foreground">
                          {b.cancel_at_period_end ? t("billing.endsOn") : t("billing.renewsOn")}
                        </dt>
                        <dd>{formatDateTime(end())}</dd>
                      </div>
                    )}
                  </Show>
                </dl>
              </Card>
              <Show
                when={!b.subscription_open}
                fallback={
                  <div>
                    <Button variant="confirm" loading={redirect.isPending} onClick={() => go(null)}>
                      {t("billing.manage")}
                    </Button>
                  </div>
                }
              >
                <Card title={t("billing.plans")}>
                  <ul class="flex flex-col divide-y divide-border">
                    <For each={b.plans}>
                      {(plan) => (
                        <li class="flex flex-wrap items-center justify-between gap-3 px-4 py-3">
                          <span class="font-semibold text-heading">{plan}</span>
                          <Button
                            variant="confirm"
                            loading={redirect.isPending && redirect.variables === plan}
                            disabled={redirect.isPending}
                            onClick={() => go(plan)}
                          >
                            {t("billing.choose")}
                          </Button>
                        </li>
                      )}
                    </For>
                  </ul>
                </Card>
                <Show when={b.status}>
                  <div>
                    <Button
                      loading={redirect.isPending && redirect.variables === null}
                      onClick={() => go(null)}
                    >
                      {t("billing.manage")}
                    </Button>
                  </div>
                </Show>
              </Show>
            </div>
          </Show>
        )}
      </QueryState>
    </Show>
  );
}
