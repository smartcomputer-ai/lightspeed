/// Whether the app runs on the in-browser demo backend, and how to reset its
/// data. The demo entry (src/demo/main.ts) turns it on before it loads the
/// app, so the app draws demo notices itself (the welcome dialog, the reset
/// in settings) instead of markup injected into the page.

export interface DemoMode {
  /// Starts the demo over from its seeded data.
  reset: () => void;
}

let current: DemoMode | null = null;

export function enableDemoMode(mode: DemoMode): void {
  current = mode;
}

/// The demo mode, or null in a real deployment.
export function demoMode(): DemoMode | null {
  return current;
}
