import { PageHeader } from "@/components/page";
import { Todo } from "@/components/todo";
import { DemoModeNote } from "@/components/onboarding/demo-mode-note";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { demoMode } from "@/lib/demo-mode";
import { useTheme, type Theme } from "@/lib/theme";
import { DarkThemeIcon, LightThemeIcon, SystemThemeIcon } from "@/icons";

/// The signed-in person's own preferences, above any universe like the
/// account page.
export function PreferencesPage() {
  return (
    <>
      <PageHeader title="Preferences" description="How Lightspeed looks and works for you." />
      <div className="grid gap-6">
        <ThemeCard />
        <DemoDataCard />
        <Todo description="Your personal preferences: which notifications reach you, defaults for new sessions, and similar settings." />
      </div>
    </>
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

function ThemeCard() {
  const { theme, setTheme } = useTheme();
  const options: { value: Theme; label: string; icon: typeof LightThemeIcon }[] = [
    { value: "light", label: "Light", icon: LightThemeIcon },
    { value: "dark", label: "Dark", icon: DarkThemeIcon },
    { value: "system", label: "System", icon: SystemThemeIcon },
  ];

  return (
    <Card id="appearance" className="scroll-mt-6">
      <CardHeader>
        <CardTitle>Appearance</CardTitle>
      </CardHeader>
      <CardContent className="flex gap-2">
        {options.map(({ value, label, icon: Icon }) => (
          <Button
            key={value}
            variant={theme === value ? "secondary" : "outline"}
            onClick={() => setTheme(value)}
          >
            <Icon data-icon="inline-start" />
            {label}
          </Button>
        ))}
      </CardContent>
    </Card>
  );
}
