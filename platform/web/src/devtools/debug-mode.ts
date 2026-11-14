/// Debug mode adds the Ctrl+D toolbar (element feedback, live design-token
/// editing) to any build. It is off unless the build sets
/// `VITE_ENABLE_DEBUG_MODE=true`, for example in `platform/web/.env.local`
/// or on the command line: `VITE_ENABLE_DEBUG_MODE=true npm run dev`.
/// The value is fixed at build time; without it the toolbar's lazily loaded
/// chunk is never requested.
export const debugModeEnabled = /^(true|1|yes|on)$/i.test(import.meta.env.VITE_ENABLE_DEBUG_MODE ?? "");
