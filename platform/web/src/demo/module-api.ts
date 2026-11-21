/** Browser demo services used by installed module route factories. */
export { demoCanReadSession, demoUniverseActions } from "./access";
export { demoAccess, newSession } from "./engine";
export { asRecord, isRecord } from "./input";
export { badRequest, conflict, intQuery, notFound, readBody, universeFor } from "./routes/common";
export { resolveSessionEnvironment, sessionConfig } from "./session-config";
export { sessionSummary, type DemoStore, type UniverseState } from "./store";
