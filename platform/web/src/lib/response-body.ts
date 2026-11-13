/// Reading an answer that should be JSON but may not be. Whatever stands in
/// front of the API (a reverse proxy, an ingress, a load balancer, a sign-in
/// portal) answers a timeout or an outage with plain text or an HTML page, and
/// the person looking at the screen needs that text, not a parse error.

/** Enough to name the failure without printing a whole error page. */
const EXCERPT_LIMIT = 300;

export type ResponseBody =
  | { kind: "json"; value: unknown }
  | { kind: "empty" }
  /** Not JSON: the text as sent, shortened for display. */
  | { kind: "text"; text: string };

/// Decided by parsing, not by the content type: a wrong or missing label must
/// not hide a valid answer, and a JSON label on a broken body must not throw.
export async function readResponseBody(res: Response): Promise<ResponseBody> {
  const raw = await res.text();
  if (raw.trim() === "") return { kind: "empty" };
  try {
    return { kind: "json", value: JSON.parse(raw) as unknown };
  } catch {
    return { kind: "text", text: excerpt(raw) };
  }
}

/** A failed request answered with text, for example `504: upstream request timeout`. */
export function failureText(status: number, text: string): string {
  return `${status}: ${text}`;
}

/** A successful status whose body is not the JSON the caller needs. */
export function notJsonText(status: number, text: string): string {
  return `Unexpected response (${status}), not JSON: ${text}`;
}

function excerpt(raw: string): string {
  const line = raw.replace(/\s+/g, " ").trim();
  return line.length > EXCERPT_LIMIT ? `${line.slice(0, EXCERPT_LIMIT)}…` : line;
}
