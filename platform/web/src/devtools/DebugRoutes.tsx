import { Navigate, Route, Routes } from "react-router-dom";
import { UiOverviewPage } from "./UiOverviewPage";

/// Standalone tool pages under /debug. They render outside the app shell and
/// need no session, so they also work against a server that is down. App
/// loads this module only in development builds.
export default function DebugRoutes() {
  return (
    <Routes>
      <Route path="debug/ui-overview" element={<UiOverviewPage />} />
      <Route path="*" element={<Navigate to="/" replace />} />
    </Routes>
  );
}
