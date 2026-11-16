import {
  createContext,
  useContext,
  useEffect,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { DEFAULT_THEME, SETTINGS, useSetting, type Theme } from "@/lib/settings";
import { applyReducedMotion } from "@/lib/reduced-motion";

export { DEFAULT_THEME, type Theme };

const ThemeContext = createContext<{
  theme: Theme;
  setTheme: (theme: Theme) => void;
}>({ theme: DEFAULT_THEME, setTheme: () => {} });

function apply(theme: Theme) {
  const dark =
    theme === "dark" ||
    (theme === "system" && window.matchMedia("(prefers-color-scheme: dark)").matches);
  document.documentElement.classList.toggle("dark", dark);
}

/// Class-strategy dark mode, dark unless the user picks otherwise. index.html
/// applies the stored choice inline before first paint (no flash); this
/// provider owns it from then on, follows a choice made in another tab, and
/// tracks OS changes while in "system". It keeps the reduced motion choice
/// applied to the page the same way.
export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setTheme] = useSetting(SETTINGS.theme);

  useEffect(() => applyReducedMotion(), []);

  useEffect(() => {
    apply(theme);
    if (theme !== "system") {
      return;
    }
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onChange = () => apply("system");
    media.addEventListener("change", onChange);
    return () => media.removeEventListener("change", onChange);
  }, [theme]);

  return (
    <ThemeContext.Provider value={{ theme, setTheme }}>{children}</ThemeContext.Provider>
  );
}

export function useTheme() {
  return useContext(ThemeContext);
}

function subscribeToDarkClass(onChange: () => void) {
  const observer = new MutationObserver(onChange);
  observer.observe(document.documentElement, { attributes: true, attributeFilter: ["class"] });
  return () => observer.disconnect();
}

const isDarkNow = () => document.documentElement.classList.contains("dark");

/// Whether the page is drawn dark right now: follows the class on <html>,
/// whatever sets it (the choice, the OS while in "system", the design editor).
export function useIsDark(): boolean {
  return useSyncExternalStore(subscribeToDarkClass, isDarkNow);
}
