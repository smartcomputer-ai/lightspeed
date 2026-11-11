/// Demo entry: install the in-browser backend, then load the real app.
/// `src/main.tsx` and everything under it are untouched by demo mode.
import { mountBanner } from "./banner";
import { installDemoFetch } from "./fetch";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import { debugModeEnabled } from "@/devtools/debug-mode";

const store = createDemoStore();
// Debug commands (`debug: …` chat messages) come with debug mode, and only
// then is their code loaded.
if (debugModeEnabled) store.debug = (await import("./debug-commands")).debugTurn;
installDemoFetch(createDemoRouter(store));
mountBanner();
await import("../main");
