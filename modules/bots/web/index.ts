import { lazy } from "react";
import type { UniverseWebModule } from "@lightspeed-ai/platform-web/modules";
import { BotIcon } from "@lightspeed-ai/platform-web/modules/ui";
import { bots } from "../module";
import { isBotController } from "./lifecycle-controller";
import { BotSessionControllerPresentation } from "./components/session-controller-presentation";

const Page = lazy(() => import("./BotsPage").then(({ BotsPage }) => ({ default: BotsPage })));
const Activity = lazy(() => import("./BotsPage").then(({ BotActivityPage }) => ({ default: BotActivityPage })));

export default {
  id: bots.id,
  label: bots.name,
  icon: BotIcon,
  action: "read",
  requiresFeature: bots.requiresFeature,
  masterDetail: true,
  routes: [
    { path: "", Page },
    { path: ":botId", Page, mobileDetail: true },
    { path: ":botId/chat/:sessionId", Page, mobileDetail: true },
    { path: ":botId/activity", Page: Activity, mobileDetail: true },
  ],
  sessionController: {
    matches: session => isBotController(session.integration),
    Presenter: BotSessionControllerPresentation,
  },
} satisfies UniverseWebModule;
