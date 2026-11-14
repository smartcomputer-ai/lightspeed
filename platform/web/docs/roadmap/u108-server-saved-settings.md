# U108: Server-Saved Settings

Status: [todo]

In the live version, user settings are currently saved only in the browser's `localStorage`. This means they don't follow the user to another browser or device, and they are lost when site data is cleared. Only the profile name and password are saved on the server (through the auth client).

Move these settings to live-server-based saving, so they are stored per account by the management server and load wherever the user signs in:

- Display preferences (`lib/user-preferences.tsx`): run statistics, session resources, collapse completed runs, sidebar width and collapsed state, list width, and keep meta toolbars expanded.
- Theme (`lib/theme.tsx`). This is currently stored per browser, not per account.
- Session list sort and metadata filter for each universe (`lib/sessions/list-preferences.ts`).
- Last opened universe (`lib/universes.ts`).
- Welcome dialog "Don't show again" (`lib/welcome.ts`), so onboarding is not shown again on a new device.

Out of scope, and fine to keep in browser storage: the debug toolbar state, the devtools design overrides, composer drafts, and composer model choice and attachments.

Requirements:

- Settings apply immediately in the UI and save in the background. A failed save must not block the UI or reset the setting; retry it later.
- The account on the server holds the saved value, and each device keeps a cached copy for fast access. Most settings need this cache wherever possible, and the new system should be built around it: the UI reads the cached copy right away (for example on first paint, so nothing jumps or flashes), then updates both the UI and the cache when the server value arrives. Keep the current `localStorage` values as this cache.
- On first load after this change, move any existing local values to the server once, so users keep their current settings.
- Changes still sync between open tabs, and they reach other devices on the next load.
- Unknown or invalid stored values fall back to the defaults, like they do now.
- Demo mode keeps saving these settings to browser storage, since it has no server. Only the live version saves them to the user account.

## Theme

Common practice is to save the theme with the account and also keep a copy on the device. Most large apps sync the theme with the account, because users expect their settings to follow them when they sign in elsewhere. Keeping it only on the device is simpler, but then the setting looks lost on every new machine.

Best practices to follow:

- Offer a "System" option that follows the operating system's light or dark setting. This covers the main reason for a per-device theme (for example, dark on a laptop at night and light on a phone) without separate settings per device. `lib/theme.tsx` already supports `light`, `dark` and `system`.
- Save the user's choice to the account, so it carries over to new browsers and devices.
- Keep a local copy and apply it before the page draws. The server answers too late for the first paint, so a theme read only from the server makes the page flash the wrong colours. `index.html` already applies the stored theme before first paint, and that has to keep working.
- Use the device's copy, or the default, when nobody is signed in, for example on the sign-in page.
- If the theme from the account differs from the local copy after sign-in, switch to the account's theme and update the local copy.

The default theme stays dark (`DEFAULT_THEME`), even though many apps default to "System". Dark is part of the Lightspeed branding.
