/// <reference types="vite/client" />

interface ImportMetaEnv {
  /// "true" turns on the debug toolbar (Ctrl+D); see src/devtools/debug-mode.ts.
  readonly VITE_ENABLE_DEBUG_MODE?: string;
}
