import { useEffect } from "react";
import { keepDialogStarscapeWarm } from "./prewarm";

/// Keeps a dialog starscape drawn ahead while the caller (the app shell) is
/// shown, so creating dialogs open on it without a stall (see prewarm.ts).
/// With reduced motion too: the dialog then shows it still.
export function useDialogStarscapeWarmth() {
  useEffect(() => keepDialogStarscapeWarm(), []);
}
