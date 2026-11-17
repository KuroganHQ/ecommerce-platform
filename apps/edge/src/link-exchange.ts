import { platformSegment } from "@platform/theme-kit";

export const LINK_GRANT_COOKIE = "__Host-Http-cap";
export const LINK_GRANT_HEADER = "x-link-grant";
export const REOPEN_LINK_PATH = "/o";

const STRIPE_RETURN_PARAMETERS = new Set([
  "payment_intent",
  "payment_intent_client_secret",
  "setup_intent",
  "setup_intent_client_secret",
  "redirect_status",
  "source",
  "client_secret",
]);
const SECRET_PARAMETERS = new Set(["token", "t", "code", ...STRIPE_RETURN_PARAMETERS]);

const ORDER_LINK = /^\/o\/([0-9a-f]{64})$/;
const ORDER_PAGE = /^\/o\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export const WITHDRAWAL_PAGE = "/withdraw/declaration";
export const REVIEW_PAGE = "/review";

type ActionKind =
  | "withdrawal"
  | "review"
  | "newsletter_confirmation"
  | "newsletter_preferences"
  | "watch_confirmation"
  | "watch_unsubscribe"
  | "flow_unsubscribe"
  | "restore_cart";

const ACTION_LINKS: Record<ActionKind, { page: string; parameter: string; clean?: string }> = {
  withdrawal: { page: "/withdraw", parameter: "t", clean: WITHDRAWAL_PAGE },
  review: { page: REVIEW_PAGE, parameter: "token" },
  newsletter_confirmation: { page: "/newsletter/confirm", parameter: "token" },
  newsletter_preferences: { page: "/newsletter", parameter: "t" },
  watch_confirmation: { page: "/watch/confirm", parameter: "token" },
  watch_unsubscribe: { page: "/watch/unsubscribe", parameter: "token" },
  flow_unsubscribe: { page: "/flows/unsubscribe", parameter: "token" },
  restore_cart: { page: "/restore-cart", parameter: "token" },
};
const ACTION_PAGES = new Map(
  Object.entries(ACTION_LINKS).map(([kind, a]) => [a.page, { ...a, kind: kind as ActionKind }]),
);
const LINK_TOKEN = /^[0-9a-f]{64}$/;

export type Link = { kind: "order" | ActionKind; token: string };
export type LinkIngress =
  | { kind: "exchange"; link: Link }
  | { kind: "redirect"; location: string }
  | { kind: "refuse" };

const CANONICAL_PATH = /^(?:\/[a-z0-9-]+)+$/;

const isCanonicalPlatformPath = (pathname: string) =>
  CANONICAL_PATH.test(pathname) && pathname.startsWith(`/${platformSegment(pathname)}`);

const isSecret = (name: string) => SECRET_PARAMETERS.has(name.toLowerCase());

export const isOrderPage = (pathname: string) => ORDER_PAGE.test(pathname);

export function linkIngress(url: URL): LinkIngress | null {
  const secrets = [...url.searchParams.keys()].some(isSecret);
  if (url.pathname.startsWith("/o/") && !isOrderPage(url.pathname)) {
    const token = ORDER_LINK.exec(url.pathname)?.[1];
    return token && !secrets
      ? { kind: "exchange", link: { kind: "order", token } }
      : { kind: "redirect", location: REOPEN_LINK_PATH };
  }
  const action = ACTION_PAGES.get(url.pathname);
  if (action && url.searchParams.has(action.parameter)) {
    const tokens = url.searchParams.getAll(action.parameter);
    const others = [...url.searchParams.keys()].filter(
      (name) => name !== action.parameter && isSecret(name),
    );
    const token = tokens.length === 1 && others.length === 0 ? tokens[0] : undefined;
    return token && LINK_TOKEN.test(token)
      ? { kind: "exchange", link: { kind: action.kind, token } }
      : { kind: "redirect", location: failedLink(action.kind) };
  }
  if (!secrets) return null;
  if (!isCanonicalPlatformPath(url.pathname)) return { kind: "refuse" };
  const kept = new URLSearchParams([...url.searchParams].filter(([name]) => !isSecret(name)));
  if ([...url.searchParams.keys()].some((name) => STRIPE_RETURN_PARAMETERS.has(name.toLowerCase())))
    kept.set("returned", "1");
  const query = kept.toString();
  return { kind: "redirect", location: query ? `${url.pathname}?${query}` : url.pathname };
}

export const failedLink = (kind: Link["kind"]) =>
  kind === "order" ? REOPEN_LINK_PATH : `${ACTION_LINKS[kind].page}?link=invalid`;

export function cleanUrl(resource: unknown): string | null {
  const r = resource as { kind?: unknown; id?: unknown } | null | undefined;
  const action =
    typeof r?.kind === "string" && Object.hasOwn(ACTION_LINKS, r.kind)
      ? ACTION_LINKS[r.kind as ActionKind]
      : null;
  if (action) return action.clean ?? action.page;
  if (r?.kind !== "order" || typeof r.id !== "string") return null;
  const path = `/o/${r.id}`;
  return isOrderPage(path) ? path : null;
}

export const NEWSLETTER_CLICK = "/newsletter/click";
export const ONE_CLICK_PREFIX = "/_p/unsubscribe/";
export const RESTORE_PAGE = "/restore-cart";

export interface GrantForm {
  api: string;
  page: string;
  done: string;
}

export const GRANT_FORMS: ReadonlyMap<string, GrantForm> = new Map([
  [
    "/newsletter/confirm",
    {
      api: "/link-grants/newsletter-confirmation",
      page: "/newsletter/confirm",
      done: "/newsletter/confirm?done=1",
    },
  ],
  [
    "/newsletter/unsubscribe",
    {
      api: "/link-grants/newsletter-preferences/unsubscribe",
      page: "/newsletter",
      done: "/newsletter?done=unsubscribed",
    },
  ],
  [
    "/newsletter/resubscribe",
    {
      api: "/link-grants/newsletter-preferences/resubscribe",
      page: "/newsletter",
      done: "/newsletter?done=resubscribed",
    },
  ],
  [
    "/watch/confirm",
    {
      api: "/link-grants/watch-confirmation",
      page: "/watch/confirm",
      done: "/watch/confirm?done=1",
    },
  ],
  [
    "/watch/unsubscribe",
    {
      api: "/link-grants/watch-unsubscribe",
      page: "/watch/unsubscribe",
      done: "/watch/unsubscribed",
    },
  ],
  [
    "/flows/unsubscribe",
    {
      api: "/link-grants/flow-unsubscribe",
      page: "/flows/unsubscribe",
      done: "/flows/unsubscribed",
    },
  ],
  [RESTORE_PAGE, { api: "/link-grants/restore-cart", page: RESTORE_PAGE, done: "/checkout" }],
]);

export const GRANT_PAGES: ReadonlySet<string> = new Set([
  ...Object.values(ACTION_LINKS)
    .filter((a) => !a.clean && a.page !== REVIEW_PAGE)
    .map((a) => a.page),
  "/watch/unsubscribed",
  "/flows/unsubscribed",
]);

export const grantCookie = (grant: string) =>
  `${LINK_GRANT_COOKIE}=${grant}; Path=/; HttpOnly; Secure; SameSite=Lax`;

export const REOPEN_LINK_PAGE = `<!doctype html><html lang="cs"><meta charset="utf-8"><meta name="robots" content="noindex">
<title>Odkaz už neplatí</title>
<main><h1>Odkaz už neplatí</h1>
<p>Tato stránka je dostupná jen chvíli po otevření odkazu a jen v prohlížeči, ve kterém jste ho otevřeli. Otevřete prosím odkaz z e-mailu znovu.</p>
<p lang="en">This page is only available for a short time after you open its link, and only in that browser. Please open the link from your email again.</p></main></html>`;
