import type { Server as HttpServer } from "node:http";
import type { AddressInfo } from "node:net";
import { LightspeedRpcError, LightspeedTransportError } from "@lightspeed/agent-client";
import { StreamableHTTPServerTransport } from "@modelcontextprotocol/sdk/server/streamableHttp.js";
import express, {
  type ErrorRequestHandler,
  type Express,
  type Request,
  type Response,
} from "express";
import type { ConfiguratorConfig } from "./config.js";
import { createToolRegistry, type ToolRegistry } from "./mcp-server.js";
import { authenticateHeaders, HttpAuthError } from "./request-auth.js";
import {
  createUpstreamClientFactory,
  type UpstreamClientFactory,
  validateUpstreamIdentity,
} from "./upstream-client.js";

export interface ConfiguratorAppOptions {
  config: ConfiguratorConfig;
  fetch?: typeof fetch;
  upstreamFactory?: UpstreamClientFactory;
}

export interface RunningConfigurator {
  app: Express;
  server: HttpServer;
  url: string;
  close(): Promise<void>;
}

export function createConfiguratorApp(options: ConfiguratorAppOptions): Express {
  const { config } = options;
  const upstreamFactory =
    options.upstreamFactory ??
    createUpstreamClientFactory({
      endpoint: config.rpcEndpoint,
      ...(options.fetch === undefined ? {} : { fetch: options.fetch }),
      maxResponseBytes: config.maxBodyBytes,
    });
  const registry = createToolRegistry(upstreamFactory, config.upstreamTimeoutMs);
  const app = express();
  app.use((req, res, next) => {
    try {
      validateHost(req, config.allowedHosts);
      validateOrigin(req, config.allowedOrigins);
      next();
    } catch (error) {
      sendHttpError(res, error);
    }
  });
  app.use(express.json({ limit: config.maxBodyBytes }));

  app.get("/health", (_req, res) => {
    res.type("text/plain").send("ok");
  });

  app.post("/mcp", async (req, res) => {
    await handleMcpPost(req, res, config, registry, upstreamFactory);
  });

  app.get("/mcp", methodNotAllowed);
  app.delete("/mcp", methodNotAllowed);
  const bodyErrorHandler: ErrorRequestHandler = (error, _req, res, next) => {
    if (isBodyTooLarge(error)) {
      res.status(413).json({
        jsonrpc: "2.0",
        error: { code: -32000, message: "Request body is too large" },
        id: null,
      });
      return;
    }
    next(error);
  };
  app.use(bodyErrorHandler);
  return app;
}

export async function startConfigurator(
  options: ConfiguratorAppOptions,
): Promise<RunningConfigurator> {
  const app = createConfiguratorApp(options);
  const server = await new Promise<HttpServer>((resolve, reject) => {
    const listening = app.listen(options.config.bindPort, options.config.bindHost, () => {
      resolve(listening);
    });
    listening.once("error", reject);
  });
  const address = server.address() as AddressInfo;
  const host = address.address.includes(":") ? `[${address.address}]` : address.address;
  return {
    app,
    server,
    url: `http://${host}:${address.port}`,
    close: () => closeServer(server, options.config.shutdownTimeoutMs),
  };
}

async function handleMcpPost(
  req: Request,
  res: Response,
  config: ConfiguratorConfig,
  registry: ToolRegistry,
  upstreamFactory: UpstreamClientFactory,
): Promise<void> {
  const abort = new AbortController();
  const onAborted = () => abort.abort(new Error("client request aborted"));
  const onResponseClose = () => {
    if (!res.writableEnded) {
      abort.abort(new Error("client connection closed"));
    }
  };
  req.once("aborted", onAborted);
  res.once("close", onResponseClose);

  try {
    const auth = authenticateHeaders(config.authMode, req.headers);
    await validateUpstreamIdentity(
      upstreamFactory,
      auth,
      abort.signal,
      config.upstreamTimeoutMs,
    );

    const mcpServer = registry.createServer(auth);
    const transport = new StreamableHTTPServerTransport({
      enableJsonResponse: true,
    });
    await mcpServer.connect(transport as Parameters<typeof mcpServer.connect>[0]);
    res.once("close", () => {
      void transport.close();
      void mcpServer.close();
    });
    await transport.handleRequest(req, res, req.body);
  } catch (error) {
    if (!res.headersSent) {
      sendHttpError(res, error);
    }
  } finally {
    req.off("aborted", onAborted);
    res.off("close", onResponseClose);
  }
}

function validateOrigin(req: Request, allowedOrigins: readonly string[]): void {
  const origin = req.header("origin");
  if (origin === undefined) {
    return;
  }
  if (!allowedOrigins.includes(origin)) {
    throw new HttpAuthError(403, "Origin is not allowed");
  }
}

function validateHost(req: Request, allowedHosts: readonly string[]): void {
  const raw = req.header("host");
  if (raw === undefined) {
    throw new HttpAuthError(400, "missing Host header");
  }
  let hostname: string;
  try {
    hostname = new URL(`http://${raw}`).hostname;
  } catch {
    throw new HttpAuthError(400, "invalid Host header");
  }
  const normalized = hostname.startsWith("[") ? hostname.slice(1, -1) : hostname;
  const allowed = allowedHosts.some((entry) => {
    const candidate = entry.startsWith("[") ? entry.slice(1, -1) : entry;
    return candidate.toLowerCase() === normalized.toLowerCase();
  });
  if (!allowed) {
    throw new HttpAuthError(403, "Host is not allowed");
  }
}

function sendHttpError(res: Response, error: unknown): void {
  const status = httpStatus(error);
  const message =
    error instanceof HttpAuthError
      ? error.message
      : status === 401
        ? "invalid Lightspeed credentials"
        : "Configurator MCP request failed";
  if (status === 401) {
    res.setHeader("www-authenticate", 'Bearer realm="lightspeed-configurator"');
  }
  res.status(status).json({
    jsonrpc: "2.0",
    error: { code: status === 400 ? -32600 : -32000, message },
    id: null,
  });
}

function httpStatus(error: unknown): number {
  if (error instanceof HttpAuthError) {
    return error.status;
  }
  if (error instanceof LightspeedTransportError) {
    return error.status === 401 || error.status === 403 ? error.status : 502;
  }
  if (error instanceof LightspeedRpcError) {
    switch (error.data?.kind) {
      case "rejected":
        return 401;
      case "not_found":
        return 404;
      case "invalid_request":
        return 400;
      default:
        return 502;
    }
  }
  return 500;
}

function methodNotAllowed(_req: Request, res: Response): void {
  res.status(405).json({
    jsonrpc: "2.0",
    error: { code: -32000, message: "Method not allowed" },
    id: null,
  });
}

function isBodyTooLarge(error: unknown): boolean {
  return (
    typeof error === "object" &&
    error !== null &&
    "type" in error &&
    error.type === "entity.too.large"
  );
}

function closeServer(server: HttpServer, timeoutMs: number): Promise<void> {
  return new Promise((resolve, reject) => {
    const timeout = setTimeout(() => server.closeAllConnections(), timeoutMs);
    timeout.unref();
    server.close((error) => {
      clearTimeout(timeout);
      if (error) {
        reject(error);
      } else {
        resolve();
      }
    });
  });
}
