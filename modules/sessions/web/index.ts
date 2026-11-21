import { lazy } from "react";
import { sessions } from "../module";
import { SessionIcon } from "@lightspeed-ai/platform-web/modules/ui";
import type { UniverseWebModule } from "@lightspeed-ai/platform-web/modules";

const Page = lazy(() => import("./SessionsPage").then(({ SessionsPage }) => ({ default: SessionsPage })));

const sessionsModule: UniverseWebModule = {
  id: sessions.id,
  label: sessions.name,
  icon: SessionIcon,
  action: "read",
  masterDetail: true,
  sessionHref: (handle, sessionId) => `/u/${encodeURIComponent(handle)}/sessions/${encodeURIComponent(sessionId)}`,
  routes: [
    { path: "", Page },
    { path: ":sessionId", Page, mobileDetail: true },
  ],
};

export default sessionsModule;
