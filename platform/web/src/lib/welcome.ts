/// Whether the welcome dialog was turned off ("Don't show anymore"), kept in
/// local storage. Resetting the demo turns it back on.

const DISMISSED_KEY = "lightspeed:welcome-dismissed";

/// Whether the welcome was turned off; storage that cannot be read counts as not.
export function welcomeDismissed(): boolean {
  try {
    return window.localStorage.getItem(DISMISSED_KEY) === "true";
  } catch {
    return false;
  }
}

export function dismissWelcome(): void {
  try {
    window.localStorage.setItem(DISMISSED_KEY, "true");
  } catch {
    // Without storage it simply shows again next time.
  }
}

/// Shows the welcome again on the next load.
export function resetWelcome(): void {
  try {
    window.localStorage.removeItem(DISMISSED_KEY);
  } catch {
    // Nothing stored, nothing to reset.
  }
}
