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

const ACTION_LINKS = new Map([
  ["/withdraw", { parameter: "t", kind: "withdrawal", page: WITHDRAWAL_PAGE }],
  ["/review", { parameter: "token", kind: "review", page: REVIEW_PAGE }],
] as const);
const LINK_TOKEN = /^[0-9a-f]{64}$/;

export type Link = { kind: "order" | "withdrawal" | "review"; token: string };
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
  const action = ACTION_LINKS.get(url.pathname);
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
  kind === "order"
    ? REOPEN_LINK_PATH
    : `${kind === "review" ? REVIEW_PAGE : WITHDRAWAL_PAGE}?link=invalid`;

export function cleanUrl(resource: unknown): string | null {
  const r = resource as { kind?: unknown; id?: unknown } | null | undefined;
  if (r?.kind === "withdrawal") return WITHDRAWAL_PAGE;
  if (r?.kind === "review") return REVIEW_PAGE;
  if (r?.kind !== "order" || typeof r.id !== "string") return null;
  const path = `/o/${r.id}`;
  return isOrderPage(path) ? path : null;
}

export const grantCookie = (grant: string) =>
  `${LINK_GRANT_COOKIE}=${grant}; Path=/; HttpOnly; Secure; SameSite=Lax`;

export const REOPEN_LINK_PAGE = `<!doctype html><html lang="cs"><meta charset="utf-8"><meta name="robots" content="noindex">
<title>Odkaz už neplatí</title>
<main><h1>Odkaz už neplatí</h1>
<p>Tato stránka je dostupná jen chvíli po otevření odkazu a jen v prohlížeči, ve kterém jste ho otevřeli. Otevřete prosím odkaz z e-mailu znovu.</p>
<p lang="en">This page is only available for a short time after you open its link, and only in that browser. Please open the link from your email again.</p></main></html>`;
