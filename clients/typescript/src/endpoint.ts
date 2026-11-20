/** Accept a deployment origin or an API base without changing its deployment path. */
export function normalizeEndpoint(value: string | URL): string {
  const raw = String(value);
  if (raw !== raw.trim() || /[\u0000-\u001f\u007f?#\\]/.test(raw)) throw new TypeError("API endpoint must not contain whitespace, controls, a query, a fragment, or backslashes");
  const url = new URL(raw);
  if (url.protocol !== "http:" && url.protocol !== "https:") throw new TypeError("API endpoint must use HTTP or HTTPS");
  if (url.username || url.password || url.search || url.hash) throw new TypeError("API endpoint must not contain credentials, a query, or a fragment");
  const path = url.pathname.replace(/\/+$/, "");
  url.pathname = path.endsWith("/api/v1") ? path : `${path}/api/v1`;
  return url.toString().replace(/\/$/, "");
}
