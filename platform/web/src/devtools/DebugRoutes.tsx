import { lazy } from "react";
import { Navigate, Route, Routes } from "react-router-dom";
import { debugModeEnabled } from "./debug-mode";
import { DesignConfigurationPage } from "./DesignConfigurationPage";

// Loaded only in development builds, so production builds leave it (and the
// to-dos it shows) out.
const UiOverviewPage = import.meta.env.DEV
  ? lazy(() => import("./UiOverviewPage").then((module) => ({ default: module.UiOverviewPage })))
  : null;

/// Standalone tool pages under /debug. They render outside the app shell and
/// need no session, so they also work against a server that is down. App
/// loads this module only in development builds or with debug mode on: the
/// UI overview is development-only, the design-token editor needs debug
/// mode (app windows only apply its changes then).
export default function DebugRoutes() {
  return (
    <Routes>
      {UiOverviewPage && <Route path="debug/ui-overview" element={<UiOverviewPage />} />}
      {UiOverviewPage && <Route path="debug/ui-overview/:section" element={<UiOverviewPage />} />}
      {debugModeEnabled && <Route path="debug/design-configuration" element={<DesignConfigurationPage />} />}
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}
