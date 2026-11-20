import { DemoModeNote } from "@/components/onboarding/demo-mode-note";
import { Button } from "@/components/ui/button";
import { ButtonGroup } from "@/components/ui/button-group";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Switch } from "@/components/ui/switch";
import { demoMode } from "@/lib/demo-mode";
import { SETTINGS, useSetting, type ReducedMotion } from "@/lib/settings";
import { useTheme, type Theme } from "@/lib/theme";
import {
  DarkThemeIcon,
  LightThemeIcon,
  ReducedMotionOffIcon,
  ReducedMotionOnIcon,
  ReducedMotionSystemIcon,
  SystemThemeIcon,
} from "@/icons";

/// How Lightspeed looks and moves, kept in this browser.
export function AppearanceSettings() {
  return (
    <div className="grid gap-6">
      <AppearanceCard />
      <AccessibilityCard />
      <DemoDataCard />
    </div>
  );
}

/// In the demo: starts its scripted data over. Nothing else in the demo is
/// stored, so this is the only way back to the start short of reloading.
function DemoDataCard() {
  const demo = demoMode();
  if (!demo) return null;
  return (
    <Card id="demo-data" className="scroll-mt-6">
      <CardHeader>
        <CardTitle>Demo data</CardTitle>
        <CardDescription>Resetting starts every universe, session and bot over from the beginning.</CardDescription>
      </CardHeader>
      <CardContent>
        <DemoModeNote />
      </CardContent>
      <CardFooter>
        <Button variant="outline" onClick={demo.reset}>Reset demo data</Button>
      </CardFooter>
    </Card>
  );
}

function AppearanceCard() {
  const { theme, setTheme } = useTheme();
  const [starscape, setStarscape] = useSetting(SETTINGS.starscape);
  const options: { value: Theme; label: string; icon: typeof LightThemeIcon }[] = [
    { value: "light", label: "Light", icon: LightThemeIcon },
    { value: "dark", label: "Dark", icon: DarkThemeIcon },
    { value: "system", label: "System", icon: SystemThemeIcon },
  ];

  return (
    <Card id="appearance" className="scroll-mt-6">
      <CardHeader>
        <CardTitle>Appearance</CardTitle>
        <CardDescription>Saved in this browser.</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-6">
        <section aria-labelledby="theme-label" className="grid gap-2">
          <h3 id="theme-label" className="text-sm font-medium">Theme</h3>
          <ButtonGroup aria-labelledby="theme-label">
            {options.map(({ value, label, icon: Icon }) => (
              <Button key={value} aria-pressed={theme === value} onClick={() => setTheme(value)}>
                <Icon data-icon="inline-start" />
                {label}
              </Button>
            ))}
          </ButtonGroup>
        </section>
        <section aria-labelledby="starscape-label" className="grid gap-2">
          <h3 id="starscape-label" className="text-sm font-medium">Starscape</h3>
          <label className="flex w-fit cursor-action items-center gap-2 text-sm text-muted-foreground">
            <Switch checked={starscape} onCheckedChange={setStarscape} />
            Show the starscape in the sidebar and behind dialogs
          </label>
        </section>
      </CardContent>
    </Card>
  );
}

function AccessibilityCard() {
  const [reducedMotion, setReducedMotion] = useSetting(SETTINGS.reducedMotion);
  const options: { value: ReducedMotion; label: string; icon: typeof ReducedMotionOnIcon }[] = [
    { value: "on", label: "Reduced", icon: ReducedMotionOnIcon },
    { value: "off", label: "Normal", icon: ReducedMotionOffIcon },
    { value: "system", label: "System", icon: ReducedMotionSystemIcon },
  ];

  return (
    <Card id="accessibility" className="scroll-mt-6">
      <CardHeader>
        <CardTitle>Accessibility</CardTitle>
        <CardDescription>Saved in this browser.</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-2">
        <div>
          <h3 id="reduced-motion-label" className="text-sm font-medium">Reduced motion</h3>
          <p className="text-sm text-muted-foreground">
            Turns off animations and moving backgrounds. System follows your device's setting.
          </p>
        </div>
        <ButtonGroup aria-labelledby="reduced-motion-label">
          {options.map(({ value, label, icon: Icon }) => (
            <Button key={value} aria-pressed={reducedMotion === value} onClick={() => setReducedMotion(value)}>
              <Icon data-icon="inline-start" />
              {label}
            </Button>
          ))}
        </ButtonGroup>
      </CardContent>
    </Card>
  );
}
