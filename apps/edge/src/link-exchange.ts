export const LINK_GRANT_COOKIE = "__Host-Http-cap";
export const LINK_GRANT_HEADER = "x-link-grant";
export const REOPEN_LINK_PATH = "/o";

/** Decision C5: the query parameters that can carry a secret into a platform page URL. */
const SECRET_PARAMETERS = new Set([
  "token",
  "t",
  "code",
  "payment_intent",
  "payment_intent_client_secret",
  "setup_intent",
  "setup_intent_client_secret",
  "redirect_status",
  "source",
  "client_secret",
]);

const ORDER_LINK = /^\/o\/([0-9a-f]{64})$/;
const ORDER_PAGE = /^\/o\/[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

export type Link = { kind: "order"; token: string };
export type LinkIngress = { kind: "exchange"; link: Link } | { kind: "redirect"; location: string };

const isSecret = (name: string) => SECRET_PARAMETERS.has(name.toLowerCase());

export const isOrderPage = (pathname: string) => ORDER_PAGE.test(pathname);

/**
 * What a platform page navigation carrying a secret becomes before anything renders: an order
 * link alone is exchanged; any other `/o/<segment>` but a clean order id, or a link next to
 * another secret, fails closed to the reopen page; elsewhere the secret parameters are dropped.
 * `null` when the URL holds no secret.
 */
export function linkIngress(url: URL): LinkIngress | null {
  const secrets = [...url.searchParams.keys()].some(isSecret);
  if (url.pathname.startsWith("/o/") && !isOrderPage(url.pathname)) {
    const token = ORDER_LINK.exec(url.pathname)?.[1];
    return token && !secrets
      ? { kind: "exchange", link: { kind: "order", token } }
      : { kind: "redirect", location: REOPEN_LINK_PATH };
  }
  if (!secrets) return null;
  const kept = new URLSearchParams([...url.searchParams].filter(([name]) => !isSecret(name)));
  const query = kept.toString();
  return { kind: "redirect", location: query ? `${url.pathname}?${query}` : url.pathname };
}

/** The clean URL of an exchanged resource, built only from a well-formed id. */
export function cleanUrl(resource: unknown): string | null {
  const r = resource as { kind?: unknown; id?: unknown } | null | undefined;
  if (r?.kind !== "order" || typeof r.id !== "string") return null;
  const path = `/o/${r.id}`;
  return isOrderPage(path) ? path : null;
}

export const grantCookie = (grant: string) =>
  `${LINK_GRANT_COOKIE}=${grant}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=1800`;

export const REOPEN_LINK_PAGE = `<!doctype html><html lang="cs"><meta charset="utf-8"><meta name="robots" content="noindex">
<title>Odkaz už neplatí</title>
<main><h1>Odkaz už neplatí</h1>
<p>Tato stránka je dostupná jen chvíli po otevření odkazu a jen v prohlížeči, ve kterém jste ho otevřeli. Otevřete prosím odkaz z e-mailu znovu.</p>
<p lang="en">This page is only available for a short time after you open its link, and only in that browser. Please open the link from your email again.</p></main></html>`;
