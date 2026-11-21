import { mountPlatformWeb } from "@lightspeed-ai/platform-web/shell";
import { standardWebModules } from "./web";

mountPlatformWeb(document.getElementById("root")!, standardWebModules);
