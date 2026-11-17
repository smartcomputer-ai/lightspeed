/** Only the device review screen may survive authentication redirects. Never
 * accept an arbitrary external URL or interpret a login link as approval. */
export function loginReturnPath(search: string): string {
  const target = new URLSearchParams(search).get("returnTo");
  if (!target?.startsWith("/device")) return "/";
  const parsed = new URL(target, "https://platform.invalid");
  if (parsed.origin !== "https://platform.invalid" || parsed.pathname !== "/device") return "/";
  const code = parsed.searchParams.get("user_code");
  return `/device${code ? `?${new URLSearchParams({ user_code: code }).toString()}` : ""}`;
}
export function deviceLoginPath(search: string, automatic = false): string {
  const code = new URLSearchParams(search).get("user_code");
  const target = `/device${code ? `?${new URLSearchParams({ user_code: code }).toString()}` : ""}`;
  return `/login?${new URLSearchParams({ returnTo: target, ...(automatic ? { auto: "1" } : {}) }).toString()}`;
}
export function appReturnUrl(search: string, origin: string, base = import.meta.env.BASE_URL): string {
  return new URL(`${base.replace(/\/$/, "")}${loginReturnPath(search)}`, origin).href;
}
