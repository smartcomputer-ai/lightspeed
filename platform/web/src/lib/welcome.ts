/// Whether the welcome dialog was turned off ("Don't show anymore"), kept as
/// a setting. Resetting the demo turns it back on.

import { demoMode } from "@/lib/demo-mode";
import { SETTINGS, settings } from "@/lib/settings";

/// Whether the welcome dialog is shown at all. Only the demo shows it for
/// now, where it says what the demo is; a real deployment gets it once its
/// text is ready.
export function welcomeEnabled(): boolean {
  return demoMode() !== null;
}

/// Whether the welcome was turned off; storage that cannot be read counts as not.
export function welcomeDismissed(): boolean {
  return settings.get(SETTINGS.welcomeDismissed);
}

export function dismissWelcome(): void {
  settings.set(SETTINGS.welcomeDismissed, true).catch(() => {
    // Without storage it simply shows again next time.
  });
}

/// Shows the welcome again on the next load.
export function resetWelcome(): void {
  settings.reset(SETTINGS.welcomeDismissed).catch(() => {
    // Nothing stored, nothing to reset.
  });
}
