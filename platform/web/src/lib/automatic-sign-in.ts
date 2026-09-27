const pendingKey = "lightspeed:automatic-sign-in-pending";

// Keep an unsuccessful round trip from starting another redirect on reload
// or browser Back. A verified Platform session resets the attempt.
export function beginAutomaticSignIn(): boolean {
  try {
    if (sessionStorage.getItem(pendingKey)) return false;
    sessionStorage.setItem(pendingKey, "1");
    return true;
  } catch {
    // The manual sign-in button remains available without browser storage.
    return false;
  }
}

export function finishAutomaticSignIn(): void {
  try {
    sessionStorage.removeItem(pendingKey);
  } catch {
    // Browser storage is only a redirect-loop guard, never authentication.
  }
}
