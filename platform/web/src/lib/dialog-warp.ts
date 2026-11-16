import { useEffect, useRef, useState } from "react";
import { dialogWarpMs } from "@/components/ui/dialog";

/// A creation dialog's warp (DialogContent `starscape` and `warp`): `start`
/// sends the dialog off into the stars while what it creates is created;
/// `afterWarp` runs once the warp has built up and faded out
/// (dialogWarpMs after the start; shorter with reduced motion), the moment to close the dialog and open
/// what was created; `cancel` calls the warp off, and the dialog comes back
/// (with an error, say). Opening the dialog again starts it settled.
export function useDialogWarp(open: boolean) {
  const [warp, setWarp] = useState(false);
  const started = useRef(0);
  useEffect(() => {
    if (open) setWarp(false);
  }, [open]);
  return {
    warp,
    start: () => {
      started.current = performance.now();
      setWarp(true);
    },
    afterWarp: (then: () => void) => {
      window.setTimeout(then, Math.max(0, dialogWarpMs() - (performance.now() - started.current)));
    },
    cancel: () => setWarp(false),
  };
}
