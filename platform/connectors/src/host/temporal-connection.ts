import { readFileSync } from "node:fs";
import type { NativeConnectionOptions, TLSConfig } from "@temporalio/worker";

/**
 * How the host reaches one Temporal namespace: where, whether over TLS, and
 * how it identifies itself. The runtime reads the same `TEMPORAL_*` variables
 * with the same rules, so one set of values configures both processes.
 */
export interface TemporalConnectionConfig {
  /** `host:port`, without a scheme; a bare host when the operator wrote no port. */
  address: string;
  namespace: string;
  /** TLS settings, or null for an unencrypted connection. */
  tls: TemporalTlsConfig | null;
  /** The API key itself or the file that holds it. */
  apiKey: { value: string } | { file: string } | null;
}

/**
 * How the server is verified and, optionally, the certificate the host
 * presents. Files are named by path and read when the connection is made.
 */
export interface TemporalTlsConfig {
  /** PEM file of the CA that issued the server certificate; null uses the system's trusted roots. */
  serverCaFile: string | null;
  /** Name the server certificate is checked against instead of the host in the address. */
  serverName: string | null;
  clientCertificate: { certFile: string; keyFile: string } | null;
}

/**
 * Check the `TEMPORAL_*` settings against each other and decide whether the
 * connection uses TLS. No file is read here. A blank value counts as unset.
 *
 * TLS is on when `TEMPORAL_TLS` is `true`. When it is unset, TLS is on if any
 * TLS setting or an API key is present, or the address is an `https://` URL:
 * an API key is never sent in the clear unless `TEMPORAL_TLS` is `false`.
 */
export function parseTemporalConnection(env: NodeJS.ProcessEnv): TemporalConnectionConfig {
  const present = (name: string): string | null => {
    const value = env[name]?.trim();
    return value === undefined || value.length === 0 ? null : value;
  };
  const explicitTls = parseTlsSwitch(present("TEMPORAL_TLS"));
  const serverCaFile = present("TEMPORAL_TLS_SERVER_CA_FILE");
  const serverName = present("TEMPORAL_TLS_SERVER_NAME");
  const certFile = present("TEMPORAL_TLS_CLIENT_CERT_FILE");
  const keyFile = present("TEMPORAL_TLS_CLIENT_KEY_FILE");
  const apiKeyValue = present("TEMPORAL_API_KEY");
  const apiKeyFile = present("TEMPORAL_API_KEY_FILE");

  if (apiKeyValue !== null && apiKeyFile !== null) {
    throw new TypeError("set only one of TEMPORAL_API_KEY and TEMPORAL_API_KEY_FILE");
  }
  const apiKey =
    apiKeyValue !== null
      ? { value: validatedApiKey("TEMPORAL_API_KEY", apiKeyValue) }
      : apiKeyFile !== null
        ? { file: apiKeyFile }
        : null;
  if ((certFile === null) !== (keyFile === null)) {
    const [set, missing] =
      certFile !== null
        ? ["TEMPORAL_TLS_CLIENT_CERT_FILE", "TEMPORAL_TLS_CLIENT_KEY_FILE"]
        : ["TEMPORAL_TLS_CLIENT_KEY_FILE", "TEMPORAL_TLS_CLIENT_CERT_FILE"];
    throw new TypeError(`${set} is set without ${missing}; a client certificate needs both files`);
  }
  const clientCertificate = certFile !== null && keyFile !== null ? { certFile, keyFile } : null;
  if (serverName !== null) validateServerName(serverName);

  const { scheme, authority } = splitAddress(present("TEMPORAL_ADDRESS") ?? "localhost:7233");
  // The first setting that only makes sense over TLS. It both turns TLS on
  // when the switch is unset and names the conflict when it is off.
  const tlsSetting =
    serverCaFile !== null
      ? "TEMPORAL_TLS_SERVER_CA_FILE"
      : serverName !== null
        ? "TEMPORAL_TLS_SERVER_NAME"
        : clientCertificate !== null
          ? "TEMPORAL_TLS_CLIENT_CERT_FILE"
          : null;
  let enabledBy: string | null;
  if (explicitTls === false) {
    if (tlsSetting !== null) {
      throw new TypeError(`TEMPORAL_TLS=false contradicts ${tlsSetting}; unset one of them`);
    }
    if (scheme === "https") {
      throw new TypeError("TEMPORAL_TLS=false contradicts the https:// scheme of TEMPORAL_ADDRESS");
    }
    enabledBy = null;
  } else if (explicitTls === true) {
    enabledBy = "TEMPORAL_TLS";
  } else {
    enabledBy =
      tlsSetting ??
      (apiKeyValue !== null ? "TEMPORAL_API_KEY" : null) ??
      (apiKeyFile !== null ? "TEMPORAL_API_KEY_FILE" : null) ??
      (scheme === "https" ? "TEMPORAL_ADDRESS" : null);
  }
  if (enabledBy !== null && scheme === "http") {
    throw new TypeError(
      `${enabledBy} turns TLS on, but TEMPORAL_ADDRESS is an http:// URL; write the address as ` +
        "host:port or https://, or set TEMPORAL_TLS=false for an unencrypted connection",
    );
  }

  return {
    address: authority,
    namespace: present("TEMPORAL_NAMESPACE") ?? "default",
    tls: enabledBy === null ? null : { serverCaFile, serverName, clientCertificate },
    apiKey,
  };
}

/**
 * Read the files the configuration names and build the SDK's connection
 * options. `tls` is always set explicitly, so neither the address nor the
 * presence of an API key decides it. The result holds the private key and
 * the API key: never log it.
 *
 * Files are read once, here; a rotated certificate or key takes effect at
 * the next process start.
 */
export function temporalConnectionOptions(
  config: TemporalConnectionConfig,
  readFile: (path: string) => Buffer = (path) => readFileSync(path),
): NativeConnectionOptions {
  const read = (variable: string, path: string): Buffer => {
    try {
      return readFile(path);
    } catch (error) {
      throw new TypeError(
        `${variable}: cannot read ${path}: ${error instanceof Error ? error.message : String(error)}`,
      );
    }
  };
  const readPem = (variable: string, path: string, kind: "certificate" | "privateKey"): Buffer => {
    const bytes = read(variable, path);
    const text = bytes.toString("latin1");
    const matches =
      kind === "certificate"
        ? text.includes("-----BEGIN CERTIFICATE-----")
        : text.includes("-----BEGIN ") &&
          text.includes("PRIVATE KEY-----") &&
          !text.includes("ENCRYPTED");
    if (!matches) {
      throw new TypeError(
        `${variable}: ${path} does not hold ${
          kind === "certificate" ? "a PEM certificate" : "an unencrypted PEM private key"
        }`,
      );
    }
    return bytes;
  };

  let tls: TLSConfig | false = false;
  if (config.tls !== null) {
    tls = {};
    if (config.tls.serverCaFile !== null) {
      tls.serverRootCACertificate = readPem(
        "TEMPORAL_TLS_SERVER_CA_FILE",
        config.tls.serverCaFile,
        "certificate",
      );
    }
    if (config.tls.serverName !== null) tls.serverNameOverride = config.tls.serverName;
    if (config.tls.clientCertificate !== null) {
      tls.clientCertPair = {
        crt: readPem(
          "TEMPORAL_TLS_CLIENT_CERT_FILE",
          config.tls.clientCertificate.certFile,
          "certificate",
        ),
        key: readPem(
          "TEMPORAL_TLS_CLIENT_KEY_FILE",
          config.tls.clientCertificate.keyFile,
          "privateKey",
        ),
      };
    }
  }

  const options: NativeConnectionOptions = { address: config.address, tls };
  if (config.apiKey !== null) {
    options.apiKey =
      "value" in config.apiKey
        ? config.apiKey.value
        : validatedApiKey(
            "TEMPORAL_API_KEY_FILE",
            read("TEMPORAL_API_KEY_FILE", config.apiKey.file).toString("utf8"),
          );
  }
  return options;
}

function parseTlsSwitch(value: string | null): boolean | null {
  if (value === null) return null;
  const normalized = value.toLowerCase();
  if (normalized === "true") return true;
  if (normalized === "false") return false;
  throw new TypeError("TEMPORAL_TLS must be true or false");
}

const INVALID_ADDRESS =
  "TEMPORAL_ADDRESS must be host:port or an http:// or https:// URL, " +
  "without user information, path or query";

/**
 * Split an optional `http`/`https` scheme from the address and return the
 * `host:port` the SDK takes. The scheme says whether the operator expects
 * TLS, and gives a URL written without a port its default one, as in the
 * runtime. A bare host without a port is left to the SDK, which fills in
 * Temporal's 7233, again as the runtime does.
 */
function splitAddress(address: string): { scheme: "http" | "https" | null; authority: string } {
  const separator = address.indexOf("://");
  const scheme = separator < 0 ? null : address.slice(0, separator).toLowerCase();
  if (scheme !== null && scheme !== "http" && scheme !== "https") {
    throw new TypeError(INVALID_ADDRESS);
  }
  const authority = (separator < 0 ? address : address.slice(separator + 3)).replace(/\/$/, "");
  // The address names a server and nothing else: no user information, path,
  // query or fragment. The runtime refuses the same forms.
  if (/[\s/\\?#@]/.test(authority)) throw new TypeError(INVALID_ADDRESS);
  let url: URL;
  try {
    url = new URL(`${scheme ?? "http"}://${authority}`);
  } catch {
    throw new TypeError(INVALID_ADDRESS);
  }
  if (url.hostname === "") throw new TypeError(INVALID_ADDRESS);
  if (scheme === null) return { scheme, authority };
  // `port` is empty both when none was written and when the scheme's own was.
  const port = url.port !== "" ? url.port : scheme === "https" ? "443" : "80";
  return { scheme, authority: `${url.hostname}:${port}` };
}

/**
 * The override becomes both the verified certificate name and the request
 * authority, so anything but a bare host would fail later with an opaque
 * transport error.
 */
function validateServerName(name: string): void {
  let bareHost = false;
  try {
    const url = new URL(`https://${name}/`);
    bareHost =
      url.port === "" &&
      url.username === "" &&
      url.password === "" &&
      url.pathname === "/" &&
      url.search === "" &&
      url.hash === "" &&
      url.hostname.toLowerCase() === name.toLowerCase();
  } catch {
    bareHost = false;
  }
  if (!bareHost) {
    throw new TypeError(
      "TEMPORAL_TLS_SERVER_NAME must be a bare host name, without a scheme, port or path",
    );
  }
}

/**
 * The key travels as a bearer header. A key the header cannot carry would
 * fail in the transport with no hint at its cause, so it is refused here.
 */
function validatedApiKey(variable: string, raw: string): string {
  const key = raw.trim();
  if (!/^[\x21-\x7e]+$/.test(key)) {
    throw new TypeError(
      `${variable} must hold a non-empty key of printable ASCII characters without spaces`,
    );
  }
  return key;
}
