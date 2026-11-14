/// Configured request headers, as an MCP server and a model provider's
/// endpoint both carry them: each header is a plain value every reader of its
/// owner sees, or names the token credential whose value the runtime sends.
/// The shape is the engine's `RequestHeader`.
export type RequestHeader =
  | { type: "literal"; value: string }
  | { type: "credential"; grantId: string };

/// One editable header row. `value` is the plain value or the grant id.
export interface RequestHeaderRow {
  key: number;
  name: string;
  source: RequestHeader["type"];
  value: string;
}

/// What one owner of headers adds to the shared rules. The engine applies the
/// same lists; naming them here saves a round trip.
export interface RequestHeaderOwner {
  /// Lowercase names the owner's transport or own authentication sets.
  reservedNames: ReadonlySet<string>;
  /// Lowercase prefixes reserved in the same way.
  reservedPrefixes: readonly string[];
  /// Where to configure Authorization instead of adding it as a header.
  authorizationHint: string;
}

/// Names whose value is a secret by convention. A plain value is shown to
/// everyone who can read the header's owner, so these take a credential only.
/// The engine holds the same list.
export const CREDENTIAL_ONLY_HEADER_NAMES: ReadonlySet<string> = new Set([
  "api-key", "proxy-authorization", "x-api-key",
]);

export function isCredentialOnlyHeader(name: string): boolean {
  return CREDENTIAL_ONLY_HEADER_NAMES.has(name.trim().toLowerCase());
}

export function isReservedRequestHeader(owner: RequestHeaderOwner, name: string): boolean {
  const lower = name.trim().toLowerCase();
  return owner.reservedNames.has(lower) || owner.reservedPrefixes.some((prefix) => lower.startsWith(prefix));
}

/// Whether the editor should steer this row to a credential: the name carries
/// a secret and the owner lets it be configured at all.
export function requestHeaderNeedsCredential(owner: RequestHeaderOwner, name: string): boolean {
  return isCredentialOnlyHeader(name) && !isReservedRequestHeader(owner, name);
}

export function emptyRequestHeaderRow(rows: RequestHeaderRow[]): RequestHeaderRow {
  const key = rows.reduce((max, row) => Math.max(max, row.key), 0) + 1;
  return { key, name: "", source: "literal", value: "" };
}

export function requestHeaderRows(
  headers: Record<string, RequestHeader> | null | undefined,
): RequestHeaderRow[] {
  return Object.entries(headers ?? {}).map(([name, header], index) => ({
    key: index + 1,
    name,
    source: header.type,
    value: header.type === "literal" ? header.value : header.grantId,
  }));
}

/// Rows as the wire header map. Blank rows are dropped; anything else must be
/// complete, unique regardless of case, not reserved by its owner, and a
/// credential where the name carries a secret.
export function requestHeadersInput(owner: RequestHeaderOwner, rows: RequestHeaderRow[]): {
  headers: Record<string, RequestHeader>;
  error?: string;
} {
  const headers: Record<string, RequestHeader> = {};
  const seen = new Set<string>();
  for (const row of rows) {
    const name = row.name.trim();
    const value = row.value.trim();
    if (!name && !value) continue;
    if (!name) return { headers, error: "Give every header a name." };
    const lower = name.toLowerCase();
    if (seen.has(lower)) return { headers, error: `The header ${name} is listed more than once.` };
    seen.add(lower);
    if (isReservedRequestHeader(owner, lower)) {
      return {
        headers,
        error: lower === "authorization"
          ? owner.authorizationHint
          : `The header ${name} is set by Lightspeed and cannot be configured.`,
      };
    }
    if (!value) {
      return {
        headers,
        error: row.source === "literal"
          ? `Enter a value for the header ${name}.`
          : `Choose a credential for the header ${name}.`,
      };
    }
    if (row.source === "literal" && isCredentialOnlyHeader(lower)) {
      return {
        headers,
        error: `The header ${name} carries a secret. Save the value as a token credential on the Credentials page and choose it here.`,
      };
    }
    headers[name] = row.source === "literal"
      ? { type: "literal", value }
      : { type: "credential", grantId: value };
  }
  return { headers };
}

/// A header's value as a summary shows it: the plain value, or which
/// credential supplies it. Never a token.
export function requestHeaderSummary(header: RequestHeader): string {
  return header.type === "literal" ? header.value : `credential ${header.grantId}`;
}

/// What an MCP server reserves: its transport and protocol headers, the
/// Authorization its authentication setting produces, and Lightspeed's own.
export const MCP_SERVER_HEADER_OWNER: RequestHeaderOwner = {
  reservedNames: new Set([
    "accept", "authorization", "connection", "content-length", "content-type", "expect", "host",
    "keep-alive", "last-event-id", "mcp-protocol-version", "mcp-session-id", "proxy-authenticate",
    "proxy-authorization", "proxy-connection", "te", "trailer", "transfer-encoding", "upgrade",
  ]),
  reservedPrefixes: ["x-lightspeed-"],
  authorizationHint: "Use the Authentication setting for the Authorization header.",
};

/// What a model endpoint reserves: the transport's headers and the
/// Authorization the provider's own credential is sent in.
export const MODEL_ENDPOINT_HEADER_OWNER: RequestHeaderOwner = {
  reservedNames: new Set([
    "authorization", "connection", "content-type", "cookie", "host", "set-cookie", "transfer-encoding",
  ]),
  reservedPrefixes: [],
  authorizationHint: "The provider's API key is sent as the Authorization header; it cannot be set here.",
};
