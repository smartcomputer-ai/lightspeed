import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, describe, expect, it } from "vitest";
import {
  parseTemporalConnection,
  temporalConnectionOptions,
} from "../src/host/temporal-connection.js";

const CERTIFICATE_PEM = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
const PRIVATE_KEY_PEM = "-----BEGIN PRIVATE KEY-----\nMIIE\n-----END PRIVATE KEY-----\n";

const directory = mkdtempSync(join(tmpdir(), "lightspeed-connector-temporal-"));
afterAll(() => rmSync(directory, { recursive: true, force: true }));

function file(name: string, contents: string): string {
  const path = join(directory, name);
  writeFileSync(path, contents);
  return path;
}

describe("Temporal connection settings", () => {
  it("is the unencrypted local default when nothing is set", () => {
    const config = parseTemporalConnection({});
    expect(config).toEqual({
      address: "localhost:7233",
      namespace: "default",
      tls: null,
      apiKey: null,
    });
    // `tls: false` is passed explicitly: the SDK must not decide on its own.
    expect(temporalConnectionOptions(config)).toEqual({ address: "localhost:7233", tls: false });
  });

  it("treats blank values as unset", () => {
    expect(
      parseTemporalConnection({
        TEMPORAL_ADDRESS: "  ",
        TEMPORAL_NAMESPACE: "",
        TEMPORAL_TLS: "",
        TEMPORAL_TLS_SERVER_CA_FILE: " ",
        TEMPORAL_TLS_CLIENT_CERT_FILE: "",
        TEMPORAL_API_KEY: "",
        TEMPORAL_API_KEY_FILE: "",
      }),
    ).toEqual(parseTemporalConnection({}));
  });

  it("turns TLS on from the switch, any TLS setting, or an https address", () => {
    const plain = { serverCaFile: null, serverName: null, clientCertificate: null };
    expect(parseTemporalConnection({ TEMPORAL_TLS: "TRUE" }).tls).toEqual(plain);
    expect(
      parseTemporalConnection({ TEMPORAL_TLS_SERVER_CA_FILE: "/etc/temporal/ca.pem" }).tls,
    ).toEqual({ ...plain, serverCaFile: "/etc/temporal/ca.pem" });
    expect(
      parseTemporalConnection({ TEMPORAL_TLS_SERVER_NAME: "frontend.temporal.internal" }).tls,
    ).toEqual({ ...plain, serverName: "frontend.temporal.internal" });
    expect(
      parseTemporalConnection({
        TEMPORAL_TLS_CLIENT_CERT_FILE: "/etc/temporal/client.pem",
        TEMPORAL_TLS_CLIENT_KEY_FILE: "/etc/temporal/client.key",
      }).tls,
    ).toEqual({
      ...plain,
      clientCertificate: {
        certFile: "/etc/temporal/client.pem",
        keyFile: "/etc/temporal/client.key",
      },
    });

    const https = parseTemporalConnection({
      TEMPORAL_ADDRESS: "https://temporal.example.test:7233/",
    });
    expect(https.address).toBe("temporal.example.test:7233");
    expect(https.tls).toEqual(plain);
    expect(temporalConnectionOptions(https)).toEqual({
      address: "temporal.example.test:7233",
      tls: {},
    });

    const http = parseTemporalConnection({ TEMPORAL_ADDRESS: "http://temporal.internal:7233" });
    expect(http).toMatchObject({ address: "temporal.internal:7233", tls: null });
  });

  it("resolves an address without a port as the runtime does", () => {
    // A URL takes the port of its scheme. A bare host is passed on as written
    // and the SDK fills in Temporal's 7233, whether or not TLS is on.
    for (const [address, expected] of [
      ["temporal.internal", "temporal.internal"],
      ["temporal.internal/", "temporal.internal"],
      ["[::1]", "[::1]"],
      ["[::1]:7244", "[::1]:7244"],
      ["temporal.internal:80", "temporal.internal:80"],
      ["http://temporal.internal", "temporal.internal:80"],
      ["http://temporal.internal:80", "temporal.internal:80"],
      ["https://temporal.example.test/", "temporal.example.test:443"],
      ["HTTPS://temporal.example.test:7233", "temporal.example.test:7233"],
    ] as const) {
      expect(parseTemporalConnection({ TEMPORAL_ADDRESS: address }).address, address).toBe(
        expected,
      );
    }
    expect(
      parseTemporalConnection({ TEMPORAL_ADDRESS: "temporal.internal", TEMPORAL_TLS: "true" })
        .address,
    ).toBe("temporal.internal");
  });

  it("does not send an API key in the clear unless TLS is switched off", () => {
    const implied = parseTemporalConnection({ TEMPORAL_API_KEY: "key-1" });
    expect(implied.tls).not.toBeNull();
    expect(temporalConnectionOptions(implied)).toEqual({
      address: "localhost:7233",
      tls: {},
      apiKey: "key-1",
    });
    expect(
      parseTemporalConnection({ TEMPORAL_API_KEY_FILE: "/run/secrets/temporal" }).tls,
    ).not.toBeNull();

    const plain = parseTemporalConnection({ TEMPORAL_API_KEY: "key-1", TEMPORAL_TLS: "false" });
    expect(temporalConnectionOptions(plain)).toEqual({
      address: "localhost:7233",
      tls: false,
      apiKey: "key-1",
    });
  });

  it("refuses settings that contradict each other", () => {
    expect(() => parseTemporalConnection({ TEMPORAL_TLS: "yes" })).toThrow(
      "TEMPORAL_TLS must be true or false",
    );
    for (const setting of [
      { TEMPORAL_TLS_SERVER_CA_FILE: "/etc/temporal/ca.pem" },
      { TEMPORAL_TLS_SERVER_NAME: "frontend.internal" },
      {
        TEMPORAL_TLS_CLIENT_CERT_FILE: "/etc/temporal/client.pem",
        TEMPORAL_TLS_CLIENT_KEY_FILE: "/etc/temporal/client.key",
      },
    ]) {
      const variable = Object.keys(setting)[0];
      expect(() => parseTemporalConnection({ TEMPORAL_TLS: "false", ...setting })).toThrow(
        `TEMPORAL_TLS=false contradicts ${variable}`,
      );
    }
    expect(() =>
      parseTemporalConnection({
        TEMPORAL_TLS: "false",
        TEMPORAL_ADDRESS: "https://temporal.example.test:7233",
      }),
    ).toThrow("TEMPORAL_TLS=false contradicts the https:// scheme");

    const address = { TEMPORAL_ADDRESS: "http://temporal.internal:7233" };
    for (const [variable, value] of [
      ["TEMPORAL_TLS", "true"],
      ["TEMPORAL_TLS_SERVER_NAME", "frontend.internal"],
      ["TEMPORAL_API_KEY", "key-1"],
      ["TEMPORAL_API_KEY_FILE", "/run/secrets/temporal"],
    ] as const) {
      expect(() => parseTemporalConnection({ ...address, [variable]: value })).toThrow(
        `${variable} turns TLS on, but TEMPORAL_ADDRESS is an http:// URL`,
      );
    }
  });

  it("needs both client certificate files and a single API key source", () => {
    expect(() =>
      parseTemporalConnection({ TEMPORAL_TLS_CLIENT_CERT_FILE: "/etc/temporal/client.pem" }),
    ).toThrow("TEMPORAL_TLS_CLIENT_CERT_FILE is set without TEMPORAL_TLS_CLIENT_KEY_FILE");
    expect(() =>
      parseTemporalConnection({ TEMPORAL_TLS_CLIENT_KEY_FILE: "/etc/temporal/client.key" }),
    ).toThrow("TEMPORAL_TLS_CLIENT_KEY_FILE is set without TEMPORAL_TLS_CLIENT_CERT_FILE");
    expect(() =>
      parseTemporalConnection({
        TEMPORAL_API_KEY: "key-1",
        TEMPORAL_API_KEY_FILE: "/run/secrets/temporal",
      }),
    ).toThrow("set only one of TEMPORAL_API_KEY and TEMPORAL_API_KEY_FILE");
    for (const key of ["two words", "line\nbreak", "schlüssel"]) {
      expect(() => parseTemporalConnection({ TEMPORAL_API_KEY: key })).toThrow(
        "TEMPORAL_API_KEY must hold a non-empty key",
      );
    }
  });

  it("checks addresses and server names", () => {
    for (const address of [
      "grpc://temporal:7233",
      "https://",
      ":7233",
      "temporal:7233/path",
      "http://temporal:7233/path",
      "temporal:7233?tls=1",
      "https://temporal:7233#frontend",
      "https://user:secret@temporal:7233",
      "user@temporal:7233",
    ]) {
      expect(() => parseTemporalConnection({ TEMPORAL_ADDRESS: address }), address).toThrow(
        "TEMPORAL_ADDRESS must be host:port",
      );
    }
    for (const name of [
      "https://frontend.internal",
      "frontend.internal:7233",
      "frontend.internal/path",
      "two names",
    ]) {
      expect(() => parseTemporalConnection({ TEMPORAL_TLS_SERVER_NAME: name })).toThrow(
        "TEMPORAL_TLS_SERVER_NAME must be a bare host name",
      );
    }
    expect(
      parseTemporalConnection({ TEMPORAL_TLS_SERVER_NAME: "Frontend.Internal" }).tls?.serverName,
    ).toBe("Frontend.Internal");
  });

  it("reads the named files into explicit connection options", () => {
    const options = temporalConnectionOptions(
      parseTemporalConnection({
        TEMPORAL_ADDRESS: "temporal.internal:7233",
        TEMPORAL_TLS_SERVER_CA_FILE: file("ca.pem", CERTIFICATE_PEM),
        TEMPORAL_TLS_SERVER_NAME: "frontend.temporal.internal",
        TEMPORAL_TLS_CLIENT_CERT_FILE: file("client.pem", CERTIFICATE_PEM),
        TEMPORAL_TLS_CLIENT_KEY_FILE: file("client.key", PRIVATE_KEY_PEM),
        TEMPORAL_API_KEY_FILE: file("api-key", "key-from-file\n"),
      }),
    );
    expect(options).toEqual({
      address: "temporal.internal:7233",
      tls: {
        serverRootCACertificate: Buffer.from(CERTIFICATE_PEM),
        serverNameOverride: "frontend.temporal.internal",
        clientCertPair: {
          crt: Buffer.from(CERTIFICATE_PEM),
          key: Buffer.from(PRIVATE_KEY_PEM),
        },
      },
      apiKey: "key-from-file",
    });
  });

  it("names the setting whose file is missing, swapped, or unusable", () => {
    const missing = join(directory, "missing");
    const cert = file("ok.pem", CERTIFICATE_PEM);
    const key = file("ok.key", PRIVATE_KEY_PEM);
    const encrypted = file(
      "encrypted.key",
      "-----BEGIN ENCRYPTED PRIVATE KEY-----\nMIIE\n-----END ENCRYPTED PRIVATE KEY-----\n",
    );
    const options = (env: NodeJS.ProcessEnv) => () =>
      temporalConnectionOptions(parseTemporalConnection(env));

    expect(options({ TEMPORAL_TLS_SERVER_CA_FILE: missing })).toThrow(
      `TEMPORAL_TLS_SERVER_CA_FILE: cannot read ${missing}`,
    );
    expect(
      options({ TEMPORAL_TLS_CLIENT_CERT_FILE: cert, TEMPORAL_TLS_CLIENT_KEY_FILE: missing }),
    ).toThrow(`TEMPORAL_TLS_CLIENT_KEY_FILE: cannot read ${missing}`);
    expect(options({ TEMPORAL_API_KEY_FILE: missing })).toThrow(
      `TEMPORAL_API_KEY_FILE: cannot read ${missing}`,
    );

    expect(options({ TEMPORAL_TLS_SERVER_CA_FILE: key })).toThrow(
      "TEMPORAL_TLS_SERVER_CA_FILE: " + key + " does not hold a PEM certificate",
    );
    expect(
      options({ TEMPORAL_TLS_CLIENT_CERT_FILE: key, TEMPORAL_TLS_CLIENT_KEY_FILE: cert }),
    ).toThrow("TEMPORAL_TLS_CLIENT_CERT_FILE: " + key + " does not hold a PEM certificate");
    expect(
      options({ TEMPORAL_TLS_CLIENT_CERT_FILE: cert, TEMPORAL_TLS_CLIENT_KEY_FILE: cert }),
    ).toThrow("does not hold an unencrypted PEM private key");
    expect(
      options({ TEMPORAL_TLS_CLIENT_CERT_FILE: cert, TEMPORAL_TLS_CLIENT_KEY_FILE: encrypted }),
    ).toThrow("does not hold an unencrypted PEM private key");
    expect(options({ TEMPORAL_API_KEY_FILE: file("empty", " \n") })).toThrow(
      "TEMPORAL_API_KEY_FILE must hold a non-empty key",
    );
  });
});
