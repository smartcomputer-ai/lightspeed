/// Demo entry: install the in-browser backend, then load the real app.
/// `src/main.tsx` and everything under it are untouched by demo mode.
import { installDemoFetch } from "./fetch";
import { createDemoStore } from "./fixtures";
import { createDemoRouter } from "./router";
import { debugModeEnabled } from "@/devtools/debug-mode";
import { enableDemoMode } from "@/lib/demo-mode";
import { resetWelcome } from "@/lib/welcome";

const store = createDemoStore();
// Debug commands (`debug: …` chat messages) come with debug mode, and only
// then is their code loaded.
if (debugModeEnabled) store.debug = (await import("./debug-commands")).debugTurn;
installDemoFetch(createDemoRouter(store));
// The app says it is a demo itself (the welcome dialog, the reset in
// settings): nothing is injected into the page. The data lives in memory,
// so loading the app afresh starts it over, welcome dialog included.
enableDemoMode({
  reset: () => {
    resetWelcome();
    window.location.assign(import.meta.env.BASE_URL);
  },
});
await import("../main");
