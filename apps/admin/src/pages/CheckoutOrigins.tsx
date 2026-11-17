import {
  Alert,
  Badge,
  Button,
  Card,
  Checkbox,
  Dialog,
  EmptyState,
  PermissionDenied,
  showToast,
} from "@platform/ui";
import { createMutation, createQuery, useQueryClient } from "@tanstack/solid-query";
import { createSignal, For, Match, Show, Switch } from "solid-js";
import { PageHeader, QueryState, Th, tableClass, tdClass } from "../components/Page.tsx";
import { errorMessage, formatDateTime, t } from "../i18n/index.ts";
import { api, type Schemas, tenantHeader, unwrap } from "../lib/api.ts";
import { tenantKey, useMembership } from "../lib/me.ts";

type Origin = Schemas["CheckoutOrigin"];
type Mode = Schemas["CheckoutMode"];

type Action = { kind: "history"; origin: Origin } | { kind: "mode"; origin: Origin; to: Mode };

const ATTESTATION_VERSION = 1;

const otherMode = (m: Mode): Mode => (m === "isolated" ? "same_origin" : "isolated");

export default function CheckoutOrigins() {
  const qc = useQueryClient();
  const { can } = useMembership();
  const [action, setAction] = createSignal<Action | null>(null);
  const [attested, setAttested] = createSignal(false);
  const [error, setError] = createSignal<string>();

  const origins = createQuery(() => ({
    queryKey: tenantKey("checkout-origins"),
    queryFn: () =>
      unwrap(api.GET("/admin/v1/checkout-origins", { params: { header: tenantHeader() } })),
    enabled: can("admin"),
  }));

  const open = (a: Action) => {
    setError(undefined);
    setAttested(false);
    setAction(a);
  };
  const close = () => setAction(null);
  const done = async (title: string) => {
    close();
    await qc.invalidateQueries({ queryKey: tenantKey("checkout-origins") });
    showToast({ title, closeLabel: t("common.close") });
  };

  const history = createMutation(() => ({
    mutationFn: (v: { origin: string; attested: boolean }) =>
      unwrap(
        api.POST("/admin/v1/checkout-origins/history", {
          params: { header: tenantHeader() },
          body: {
            origin: v.origin,
            attestation_version: v.attested ? ATTESTATION_VERSION : null,
          },
        }),
      ),
    onSuccess: (o) => done(t("checkoutOrigins.historyRecorded", { origin: o.origin })),
    onError: (err) => setError(errorMessage(err)),
  }));

  const mode = createMutation(() => ({
    mutationFn: (v: { origin: string; mode: Mode }) =>
      unwrap(
        api.POST("/admin/v1/checkout-origins/mode", {
          params: { header: tenantHeader() },
          body: v,
        }),
      ),
    onSuccess: (o) => done(t(`checkoutOrigins.switched.${o.mode}`, { origin: o.origin })),
    onError: (err) => setError(errorMessage(err)),
  }));

  const submit = (e: SubmitEvent) => {
    e.preventDefault();
    const a = action();
    if (a?.kind === "history") history.mutate({ origin: a.origin.origin, attested: attested() });
    if (a?.kind === "mode") mode.mutate({ origin: a.origin.origin, mode: a.to });
  };

  const switching = () => {
    const a = action();
    return a?.kind === "mode" ? a : undefined;
  };

  const title = () => {
    const a = action();
    if (!a) return "";
    return a.kind === "history"
      ? t("checkoutOrigins.historyTitle", { origin: a.origin.origin })
      : t(`checkoutOrigins.switchTitle.${a.to}`, { origin: a.origin.origin });
  };

  return (
    <Show
      when={can("admin")}
      fallback={
        <>
          <PageHeader title={t("checkoutOrigins.title")} />
          <PermissionDenied
            title={t("common.forbiddenTitle")}
            description={t("common.forbiddenDesc")}
          />
        </>
      }
    >
      <PageHeader title={t("checkoutOrigins.title")} description={t("checkoutOrigins.lead")} />
      <QueryState query={origins}>
        {(data) => (
          <div class="flex flex-col gap-3">
            <Card title={t("checkoutOrigins.title")} count={data.items.length} padding="none">
              <Show
                when={data.items.length > 0}
                fallback={
                  <EmptyState
                    title={t("checkoutOrigins.empty")}
                    description={t("checkoutOrigins.emptyDesc")}
                  />
                }
              >
                <div class="overflow-x-auto">
                  <table class={tableClass}>
                    <thead>
                      <tr>
                        <Th>{t("checkoutOrigins.origin")}</Th>
                        <Th>{t("checkoutOrigins.mode")}</Th>
                        <Th class="text-right">{t("checkoutOrigins.generation")}</Th>
                        <Th>{t("checkoutOrigins.history")}</Th>
                        <Th>{t("checkoutOrigins.attestation")}</Th>
                        <Th srOnly>{t("common.actions")}</Th>
                      </tr>
                    </thead>
                    <tbody>
                      <For each={data.items}>
                        {(o) => (
                          <tr>
                            <td class={`${tdClass} font-semibold break-all text-heading`}>
                              {o.origin}
                            </td>
                            <td class={tdClass}>
                              <Badge tone={o.mode === "same_origin" ? "info" : "neutral"}>
                                {t(`checkoutOrigins.${o.mode}`)}
                              </Badge>
                            </td>
                            <td class={`${tdClass} figures text-right`}>{o.mode_generation}</td>
                            <td class={tdClass}>
                              <Badge tone={o.history === "unknown_or_used" ? "warning" : "success"}>
                                {t(`checkoutOrigins.histories.${o.history}`)}
                              </Badge>
                            </td>
                            <td class={`${tdClass} text-muted-foreground`}>
                              <Show when={o.attestation} fallback="—">
                                {(a) =>
                                  t("checkoutOrigins.attestedAt", {
                                    version: a().version,
                                    at: formatDateTime(a().at),
                                  })
                                }
                              </Show>
                            </td>
                            <td class={`${tdClass} text-right whitespace-nowrap`}>
                              <Button
                                category="tertiary"
                                size="small"
                                disabled={!can("owner")}
                                onClick={() => open({ kind: "history", origin: o })}
                              >
                                {t("checkoutOrigins.recordHistory")}
                                <span class="sr-only">: {o.origin}</span>
                              </Button>
                              <Button
                                category="secondary"
                                size="small"
                                class="ml-1"
                                disabled={!can("owner")}
                                onClick={() =>
                                  open({ kind: "mode", origin: o, to: otherMode(o.mode) })
                                }
                              >
                                {t(`checkoutOrigins.switchTo.${otherMode(o.mode)}`)}
                                <span class="sr-only">: {o.origin}</span>
                              </Button>
                            </td>
                          </tr>
                        )}
                      </For>
                    </tbody>
                  </table>
                </div>
              </Show>
            </Card>
            <Show when={!can("owner")}>
              <p class="text-sm text-muted-foreground">{t("checkoutOrigins.ownerOnly")}</p>
            </Show>
          </div>
        )}
      </QueryState>

      <Dialog open={action() !== null} onOpenChange={(o) => !o && close()} title={title()}>
        <form class="flex flex-col gap-4" onSubmit={submit}>
          <Switch>
            <Match when={action()?.kind === "history"}>
              <p class="text-sm">{t("checkoutOrigins.historyLead")}</p>
              <Checkbox
                label={t("checkoutOrigins.attestationText")}
                description={t("checkoutOrigins.attestationHint")}
                checked={attested()}
                onChange={setAttested}
              />
            </Match>
            <Match when={switching()}>
              {(a) => (
                <>
                  <Alert tone="warning">{t("checkoutOrigins.restartImpact")}</Alert>
                  <Show when={a().to === "isolated"}>
                    <Alert tone="warning">
                      {t("checkoutOrigins.rollbackDns", {
                        host: `checkout.${new URL(a().origin.origin).host}`,
                      })}
                    </Alert>
                  </Show>
                </>
              )}
            </Match>
          </Switch>
          <Show when={error()}>
            <Alert tone="error">{error()}</Alert>
          </Show>
          <div class="flex flex-wrap justify-end gap-2">
            <Button onClick={close}>{t("common.cancel")}</Button>
            <Show
              when={switching()}
              fallback={
                <Button type="submit" variant="confirm" loading={history.isPending}>
                  {t("checkoutOrigins.recordHistory")}
                </Button>
              }
            >
              {(a) => (
                <Button type="submit" variant="danger" loading={mode.isPending}>
                  {t(`checkoutOrigins.switchTo.${a().to}`)}
                </Button>
              )}
            </Show>
          </div>
        </form>
      </Dialog>
    </Show>
  );
}
