import { startPlatformServer } from "@lightspeed-ai/platform-backend/server";
import { standardApiModules } from "./api.js";

await startPlatformServer(standardApiModules);
