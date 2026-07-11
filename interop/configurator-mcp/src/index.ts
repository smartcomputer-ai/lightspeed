export { configFromEnv, type ConfiguratorConfig } from "./config.js";
export { createToolRegistry, type ToolRegistry } from "./mcp-server.js";
export {
  authenticateHeaders,
  HttpAuthError,
  PRINCIPAL_HEADER,
  UNIVERSE_HEADER,
  upstreamHeaders,
  type ConfiguratorAuthMode,
  type RequestAuthContext,
} from "./request-auth.js";
export {
  createConfiguratorApp,
  startConfigurator,
  type ConfiguratorAppOptions,
  type RunningConfigurator,
} from "./transport.js";
export {
  createUpstreamClientFactory,
  validateUpstreamIdentity,
  type UpstreamClientFactory,
} from "./upstream-client.js";
