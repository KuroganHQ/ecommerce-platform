export const IDENTIFY_START = "/checkout/identify/start";
export const IDENTIFY_CALLBACK = "/checkout/identify";
export const IDENTIFY_FAILED = "/checkout?error=identify_failed";
export const IDT_COOKIE = "__Host-Http-idt";

const IDT_SECONDS = 300;
const VALUE = /^[A-Za-z0-9_-]{16,128}$/;

export const idtCookie = (state: string) =>
  `${IDT_COOKIE}=${state}; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=${IDT_SECONDS}`;

export const CLEAR_IDT_COOKIE = `${IDT_COOKIE}=; Path=/; HttpOnly; Secure; SameSite=Lax; Max-Age=0`;

export const isIdentifyValue = (v: unknown): v is string => typeof v === "string" && VALUE.test(v);

export function accountOrigin(site: {
  shopOrigin?: string | undefined;
  account_host?: string | undefined;
}): string | null {
  if (!site.account_host || !site.shopOrigin) return null;
  const url = new URL(site.shopOrigin);
  url.hostname = site.account_host;
  return url.origin;
}

export function identifyUrl(account: string, state: string, shopOrigin: string): string {
  const url = new URL("/identify", account);
  url.search = new URLSearchParams({ state, origin: shopOrigin }).toString();
  return url.href;
}

export function callbackParameters(url: URL): { state: string; code: string } | null {
  const params = [...url.searchParams];
  const state = url.searchParams.getAll("state");
  const code = url.searchParams.getAll("code");
  return params.length === 2 &&
    state.length === 1 &&
    code.length === 1 &&
    isIdentifyValue(state[0]) &&
    isIdentifyValue(code[0])
    ? { state: state[0], code: code[0] }
    : null;
}
