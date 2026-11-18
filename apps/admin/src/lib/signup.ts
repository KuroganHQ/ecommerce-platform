import { createAdminClient } from "@platform/admin-client";
import { unwrap } from "./api.ts";
import { API_ORIGIN } from "./config.ts";

/** Self-signup runs before there is a session: no token, no tenant. */
const publicApi = createAdminClient({ baseUrl: API_ORIGIN });

/** `GET /signup` answers 404 unless the platform has self-signup on. */
export async function signupEnabled(): Promise<boolean> {
  const r = await publicApi.GET("/signup");
  return r.response.ok;
}

export function requestSignup(email: string) {
  return unwrap(publicApi.POST("/signup", { body: { email } }));
}
