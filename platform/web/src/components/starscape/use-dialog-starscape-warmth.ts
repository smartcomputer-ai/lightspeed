import { useEffect } from "react";
import { SETTINGS, useSetting } from "@/lib/settings";
import { keepDialogStarscapeWarm } from "./prewarm";

/// Keeps a dialog starscape drawn ahead while the caller (the app shell) is
/// shown, so creating dialogs open on it without a stall (see prewarm.ts).
/// With reduced motion too: the dialog then shows it still. None while the
/// starscape is turned off in Preferences.
export function useDialogStarscapeWarmth() {
  const [shown] = useSetting(SETTINGS.starscape);
  useEffect(() => (shown ? keepDialogStarscapeWarm() : undefined), [shown]);
}
