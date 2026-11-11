import { useLayoutEffect, useRef, useState } from "react";
import { LightspeedLogo } from "@/components/brand/lightspeed-logo";
import { Starscape } from "@/components/starscape/starscape";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { InfoBox } from "@/components/ui/info-box";
import { Dialog, DialogContent, DialogDescription, DialogTitle } from "@/components/ui/dialog";
import { demoMode } from "@/lib/demo-mode";
import { dismissWelcome, welcomeDismissed } from "@/lib/welcome";
import chargeImage from "@/assets/onboarding/electric-charge.png";
import handImage from "@/assets/onboarding/hand-guiding-agent.png";

/// The welcome dialog: shown when the app loads until someone ticks "Don't
/// show anymore". A start for onboarding that later asks for a theme,
/// preferences and defaults; for now it greets, and in the demo says what
/// the demo is.

export function WelcomeDialog() {
  const [open, setOpen] = useState(() => !welcomeDismissed());
  const [never, setNever] = useState(false);
  const close = () => {
    if (never) dismissWelcome();
    setOpen(false);
  };
  const demo = demoMode();
  return (
    <Dialog open={open} onOpenChange={(next) => { if (!next) close(); }}>
      <DialogContent showCloseButton={false} className="gap-0 overflow-hidden p-0 sm:max-w-xl">
        <WelcomeArt />
        <div className="grid gap-4 p-6">
          <div className="grid gap-2 text-left">
            <DialogTitle className="text-xl font-semibold tracking-tight">Welcome to Lightspeed.</DialogTitle>
            <DialogDescription className="text-sm text-muted-foreground">
              Lightspeed runs long-lived agents as durable workflows. They pick up where they left off and borrow
              infrastructure only when a task needs it. Profiles shape the agent, workspaces hold its work, and
              environments provide its resources.
            </DialogDescription>
          </div>
          {demo && (
            <InfoBox tone="info" title="Demo Mode">
              Everything here runs right in your browser on example data, so feel free to explore and play around.
              The dummy agents follow scripted commands instead of calling a real model and performing real thinking
              and tasks. Want a fresh start? Reset the demo data any time in the settings.
            </InfoBox>
          )}
          <div className="flex items-center justify-between gap-4 pt-1">
            <label className="flex items-center gap-2 text-xs text-muted-foreground">
              <Checkbox checked={never} onCheckedChange={(checked) => setNever(checked === true)} />
              Don't show anymore
            </label>
            <Button onClick={close} autoFocus>OK</Button>
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}

/// How far below the band's middle the logo's centre sits (1.5rem, as in
/// the stage's top offset below).
const LOGO_DROP_REM = 1.5;

/// The ls.bot hero as a landscape: the starscape around the Lightspeed
/// logo, with the hand guiding the agent and the charge between them, the
/// edges vignetted into black. The same art and proportions as the site's
/// stage, placed so the logo sits just below the band's centre (the stage
/// puts its centre 1.222 stage widths below its top), cropping a little of
/// the hand.
function WelcomeArt() {
  const band = useRef<HTMLDivElement>(null);
  // Where the logo's centre is: the band's middle across, LOGO_DROP below its
  // middle down (the stage is placed so). From the band's layout size, which
  // the dialog's opening zoom does not distort, so the starscape's vanishing
  // point lands on the logo however it was measured.
  const [center, setCenter] = useState<{ x: number; y: number }>();
  useLayoutEffect(() => {
    const node = band.current;
    if (!node || typeof ResizeObserver === "undefined") return;
    const place = () => {
      const drop = LOGO_DROP_REM * Number.parseFloat(getComputedStyle(document.documentElement).fontSize);
      setCenter({ x: node.offsetWidth / 2, y: node.offsetHeight / 2 + drop });
    };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(node);
    return () => observer.disconnect();
  }, []);
  return (
    <div ref={band} aria-hidden className="relative h-80 overflow-hidden bg-black">
      {center && <Starscape center={center} fade={{ start: 40, end: 420 }} className="absolute inset-0" />}
      <div className="absolute inset-0 bg-[radial-gradient(ellipse_60%_95%_at_50%_calc(50%+1.5rem),transparent_0%,rgb(0_0_0/0.65)_80%,black_100%)]" />
      {/* The site's stage, its width setting the scale; moved so the logo sits a little below the middle. */}
      <div className="absolute top-[calc(50%-var(--stage)*1.222+1.5rem)] left-1/2 h-[calc(var(--stage)*1.403)] w-(--stage) -translate-x-[74.36%] [--stage:10.75rem]">
        <div className="absolute top-[47.2%] left-[15.4%] aspect-[680/624] w-[116.6%] bg-[radial-gradient(ellipse_50%_50%_at_50%_50%,black_0%,transparent_100%)]" />
        <img src={chargeImage} alt="" className="absolute top-[40.1%] left-[25.7%] w-[60.4%]" />
        <img src={handImage} alt="" className="absolute top-0 left-0 w-[62.6%]" />
        <div className="absolute top-[74.2%] left-[60.2%] w-[28.3%] text-white">
          <LightspeedLogo className="h-auto w-full" />
        </div>
      </div>
    </div>
  );
}
