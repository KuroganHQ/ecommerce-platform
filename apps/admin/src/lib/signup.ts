import { createAdminClient } from "@platform/admin-client";
import { unwrap } from "./api.ts";
import { API_ORIGIN } from "./config.ts";

const publicApi = createAdminClient({ baseUrl: API_ORIGIN });

export async function signupEnabled(): Promise<boolean> {
  const r = await publicApi.GET("/signup");
  return r.response.ok;
}

export function requestSignup(email: string) {
  return unwrap(publicApi.POST("/signup", { body: { email } }));
}
