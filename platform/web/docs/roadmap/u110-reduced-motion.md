# U110: Reduced Motion

Status: [in progress]

Go through the entire UI and double-check that when a reduced motion flag is set, it is properly respected. For example animations and transitions should be turned off and especially the starscape should be static and not animated. 

Make sure that the reduced motion goes through a common library call to test for it, which can also be integrated in the user's profile as an option. The option is On, Off or System, and the default is System, which follows the browser's setting.

But all the settings come from the user's profile setting not the browser. The browser just sets the default correctly.

## Progress

The choice and the common check are done; the audit of the whole UI is not.

- Preferences has an Accessibility card with a three-way Reduced motion choice: On, Off or System (the default). It is a synced setting in the settings store (`lightspeed:reduced-motion`, stored only when it is not System), so a device can later keep its own value with the display preferences.
- `src/lib/reduced-motion.ts` is the one place that decides: `prefersReducedMotion()` outside React, `useReducedMotion()` in components, and `subscribeReducedMotion()` for code that follows changes. Each follows the choice, and the browser's setting only while the choice is System.
- The page carries the result as `data-motion="reduce"` or `"full"` on `<html>`. The inline script in `index.html` sets it before first paint, and `applyReducedMotion()` keeps it current from then on.
- The Tailwind `motion-safe:` and `motion-reduce:` variants are redefined in `src/index.css` to key on `data-motion` instead of the browser's media query, so every class using them follows the choice. The transcript entry animation and the status dot pulse use the attribute as well.
- Code that used to query the browser directly (the meta toolbar's switch animation, the transcript entry animation) now goes through the common check. The starscape and the dialog warp already did.
- Independent of reduced motion, Preferences can turn off the starscapes in the sidebar and behind creating dialogs (Appearance, Starscape). The welcome's starscape always shows.

## Open work

- Audit the whole UI for animations and transitions that ignore the choice: anything animated without a `motion-safe:` or `motion-reduce:` variant, and any CSS or library styles that still use the `prefers-reduced-motion` media query. The one left in bundled styles is shadcn's `.shimmer`, which the app does not use.
- Check that every starscape is still under reduced motion (the welcome's moves slowly on purpose).
