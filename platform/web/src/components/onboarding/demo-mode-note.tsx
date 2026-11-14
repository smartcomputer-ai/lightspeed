import { InfoBox } from "@/components/ui/info-box";

/// What the demo is, as the welcome dialog and the demo data card say it.
/// `resetHint` adds where to reset it, for places other than the reset
/// itself.
export function DemoModeNote({ resetHint = false }: { resetHint?: boolean }) {
  return (
    <InfoBox tone="info" title="Demo Mode">
      Everything here runs right in your browser on example data, so feel free to explore and play around.
      The dummy agents follow scripted commands instead of calling a real model and performing real thinking
      and tasks.{resetHint && " Want a fresh start? Reset the demo data any time in your preferences."}
    </InfoBox>
  );
}
