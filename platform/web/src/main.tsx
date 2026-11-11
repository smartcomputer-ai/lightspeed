import { lazy, StrictMode, Suspense } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { BusyCursor } from "@/components/busy-cursor";
import { BrowserRouter } from "react-router-dom";
import { TooltipProvider } from "@/components/ui/tooltip";
import { ThemeProvider } from "@/lib/theme";
import { retryRead } from "@/lib/read-errors";
import { App } from "./App.js";
import { debugModeEnabled } from "@/devtools/debug-mode";
import "./index.css";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: { retry: retryRead, refetchOnWindowFocus: false, staleTime: 30_000 },
  },
});
// The Ctrl+D debug toolbar, loaded only when debug mode is enabled.
const DebugToolbar = debugModeEnabled ? lazy(() => import("@/devtools/DebugToolbar")) : null;
const appBasename = import.meta.env.BASE_URL.replace(/\/$/, "");

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <BusyCursor />
      <ThemeProvider>
        <TooltipProvider>
          <BrowserRouter basename={appBasename}>
            <App />
          </BrowserRouter>
          {DebugToolbar && <Suspense fallback={null}><DebugToolbar /></Suspense>}
        </TooltipProvider>
      </ThemeProvider>
    </QueryClientProvider>
  </StrictMode>,
);
