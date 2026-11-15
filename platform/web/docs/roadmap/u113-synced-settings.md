# U113: Synced Settings

Status: [in progress]

User settings have no common interface. Each module reads and writes browser storage itself, with its own key, parsing, defaults and error handling. This plan puts one settings interface in front of all of them, with the storage behind it supplied by a provider. The first provider saves to browser storage. A later provider saves to the user's account through the management server ([U108](u108-server-saved-settings.md)) and keeps browser storage as a cache. A device override layer on top lets a user keep different settings on one device.

The same store also holds state that must never leave the browser, such as composer drafts and the debug tools. Each setting says how long it lasts and where it may go: synced to the account, kept on this device, or kept only while the tab is open.

Only the interface and the browser storage provider are in scope now. They have to be shaped so the server provider and the device overrides can be added later without changing the code that uses settings.

## Today

Every personal setting is in the browser. Nothing personal is saved on the server.

| Setting | Scope | Storage and key | Module |
| --- | --- | --- | --- |
| Theme (light, dark, system) | This browser, not the user | `localStorage` `lightspeed:theme`, also read by `index.html` before first paint | `src/lib/theme.tsx` |
| Display preferences: run statistics, session resources, collapse completed runs, sidebar width and collapsed state, list width, keep meta toolbars expanded | Signed-in user, in this browser | `localStorage` `lightspeed:user-preferences:<userId>`, one JSON object | `src/lib/user-preferences.tsx` |
| Session list options: show closed, sub-agents, managed sessions, session ids, metadata columns | Each universe | `localStorage` `lightspeed:sessions:list-preferences:<universeId>` | `src/lib/sessions/list-preferences.ts` |
| Session list metadata filter | Each universe | `localStorage` `lightspeed:sessions:metadata-filter:<universeId>`; the URL can also set it | `src/lib/sessions/list-preferences.ts` |
| Last opened universe | This browser | `localStorage` `lightspeed:last-universe` | `src/lib/universes.ts` |
| Welcome dialog dismissed | This browser | `localStorage` `lightspeed:welcome-dismissed` | `src/lib/welcome.ts` |
| Composer draft, model choice, attachments | Each session | `localStorage` `lightspeed:sessions:draft:[<universeId>,<sessionId>]`, plus `:run` and `:attachments` | `src/lib/sessions/draft.ts`, `src/lib/composer-model.ts`, `src/lib/composer-attachments.ts` |
| Design token overrides (debug mode) | This browser | `localStorage` `lightspeed:design-overrides`, shared live with other windows over a `BroadcastChannel` | `src/devtools/design-tokens.ts` |
| Debug toolbar open | This tab | `sessionStorage` `lightspeed:debug-toolbar` | `src/devtools/DebugToolbar.tsx` |

The server stores only per-universe settings that all members share: icon, icon colour and feature switches (`universes` in `platform/db/src/schema/platform.ts`).

Gaps this plan closes or prepares for:

- The theme belongs to the browser, not the user. Two people signed in on one browser share it.
- Display preferences follow changes made in other tabs (a `storage` listener). The theme does not, so other tabs keep the old theme until they reload.
- Each module repeats the same parsing, fallback to defaults, and protection against blocked or full storage.
- Drafts, model choices and attachments are removed when they are emptied, but stay in storage when their session is deleted.
- The sidebar component (`src/components/ui/sidebar.tsx`) still writes a `sidebar_state` cookie that nothing reads. Sidebar state lives in the display preferences.

## Common practice

- Large apps save settings with the account and keep a copy on the device. The device copy applies before the server answers, so the first paint uses the right theme and layout. When the account's value arrives, it wins and updates the device copy. GitHub saves the theme with the account; Discord syncs it across its desktop, mobile and web apps.
- A "System" option covers the main reason for a different theme per device. Lightspeed keeps dark as the default ([U108](u108-server-saved-settings.md)).
- Some apps let one device opt out of syncing. Discord's "Sync across clients" toggle keeps a different theme on, for example, a phone. The device override layer below is this idea.
- Some apps keep the theme per browser only. YouTube does this even when you are signed in, and users have to set it again on each browser.

## Settings interface

One store holds every setting. Code reads and writes settings only through it, never through browser storage directly.

Not everything in the store is a preference: a draft is working state. It shares the store because it needs the same things: a key, a default, safe parsing, protection against blocked storage, and updates across tabs.

Each setting is declared once, in a registry (for example `src/lib/settings/registry.ts`):

- Its key and its scope: the user, the user in one universe (the session list options), or one Lightspeed session (a composer draft). A setting for a universe or a session is declared once and called with its ids.
- Its persistence: `synced`, `device` or `tab` (see below). `synced` is the default.
- Its default.
- A parser that turns a stored value into a valid one, or rejects it. A missing, unknown or invalid stored value falls back to the default, like today.
- For a `synced` setting, whether a device may override it (see Device overrides). Off by default.

### Persistence

| Persistence | Lasts | Stored now | With the server provider | Settings |
| --- | --- | --- | --- | --- |
| `synced` | Follows the user to other devices, once the server provider ships | `localStorage` | Saved to the account; `localStorage` is the cache | Theme, display preferences, session list options and metadata filter, last universe, welcome dismissed |
| `device` | In this browser until site data is cleared | `localStorage` | Never sent to the server | Sidebar and list widths; composer drafts, model choices and attachments; queued steering recovery; design token overrides; the device override flags |
| `tab` | Until the tab closes | `sessionStorage` | Never sent to the server | Debug toolbar open |

- The tab mode is not called "session", because a session in Lightspeed is a conversation with an agent. `tab` is what `sessionStorage` does: it lasts as long as the browser tab.
- Until the server provider ships, `synced` and `device` behave the same. Declaring the difference now means nothing has to be sorted again later.
- `device` and `tab` settings ignore device overrides: they already belong to the device.
- Some settings change often: a draft on every keystroke, a design token while a slider is dragged. The store applies the new value in memory and notifies listeners at once. The provider may batch the writes to storage (for example once per animation frame, or after a short pause), and saves any pending writes when the page is hidden or unloaded.

### Store

```ts
interface SettingsStore {
  /// The current value from the cache, at once: what rendering and first paint use.
  get<T>(setting: Setting<T>): T;
  /// Waits until the provider has loaded, then gives the value.
  read<T>(setting: Setting<T>): Promise<T>;
  /// Applies the value at once (cache and listeners), then resolves once the
  /// provider has saved it. A failed save rejects, but the value stays.
  set<T>(setting: Setting<T>, value: T): Promise<void>;
  /// Removes the stored value, so the default applies again.
  reset<T>(setting: Setting<T>): Promise<void>;
  /// Calls back when the value changes: here, in another tab, or later from the server.
  subscribe<T>(setting: Setting<T>, listener: (value: T) => void): () => void;
  /// Resolves once the provider has loaded its values.
  ready: Promise<void>;
}
```

Reads must be available at once, because React renders synchronously and the theme has to apply before first paint. So `get` reads the cache and `read` waits for the provider. Writes are awaitable, so a caller can show progress or an error, but the UI never waits for a save before it shows the new value. A caller that does not care about the outcome does not have to await `set`.

In components, a hook wraps the store:

```ts
const [showRunStatistics, setShowRunStatistics] = useSetting(SETTINGS.showRunStatistics);
const [list, setList] = useSetting(SETTINGS.sessionList(universeId));
const [runChoice, setRunChoice] = useSetting(SETTINGS.composerRunChoice(draftKey));
```

`useSetting` subscribes through `useSyncExternalStore`, so every component using a setting updates together, including when another tab changes it.

## Providers

The store keeps values in memory and hands storage to a provider:

```ts
interface SettingsProvider {
  /// Resolves once the provider has loaded its values.
  ready: Promise<void>;
  /// The stored text now, from the provider's local copy; null when nothing is stored.
  peek(area: "local" | "tab", key: string): string | null;
  /// Saves changes; resolves when they are stored, rejects when they cannot be.
  save(changes: StoredChange[]): Promise<void>;
  /// Saves any writes still held back.
  flush(): Promise<void>;
  /// Values changed somewhere else: another tab now, the server later.
  /// Null means anything may have changed (storage was cleared).
  subscribe(onChange: (changed: { area: "local" | "tab"; key: string }[] | null) => void): () => void;
}
```

The provider answers `peek` at once from its local copy instead of loading everything up front, so `get` never waits, even for the first paint. A server provider answers `peek` from its browser storage cache. Each change names the setting's persistence, so a server provider can send only `synced` changes to the account, and may say how long the provider can hold the write back.

### Browser storage provider (now)

- Saves `synced` and `device` settings to `localStorage`, and `tab` settings to `sessionStorage`.
- Keeps today's keys, so no one loses a setting or a draft, and `index.html` keeps reading `lightspeed:theme` before first paint. The registry maps each setting to its key. Merging the keys into fewer is optional and can happen with the server provider.
- Loads at once, so `ready` resolves straight away and `get` and `read` give the same value.
- Listens for `storage` events, so every `synced` and `device` setting follows changes made in other tabs, the theme included. `tab` settings belong to one tab and do not.
- When storage is blocked or full, the value stays in memory for this page and `set` rejects. The UI keeps working, as it does today.
- Is also the provider in demo mode, which has no server.

### Server provider (later, U108)

- Saves `synced` settings to the user's account through the management server. [U108](u108-server-saved-settings.md) lists the settings that move and how.
- `device` and `tab` settings stay with the browser storage provider and never reach the server. This matches U108, which keeps drafts, composer choices and the debug tools in the browser.
- Uses the browser storage provider as its cache. On start, the store reads the cache at once for first paint, then loads the account's values, applies any that differ, and updates the cache.
- On `set`, it saves to the cache at once and to the server in the background. A failed save is retried later and never resets the value.
- On the first load after it ships, it uploads existing local values of `synced` settings once, so users keep their settings.
- Before sign-in (for example on the sign-in page) and in demo mode, only the cache applies.

Moving from the browser storage provider to the server provider changes only how the store is set up, not the code that uses settings.

## Device overrides

A user can keep settings on one device that differ from the settings saved to their account. This is a layer between the account's values and what the UI reads, and it applies only to `synced` settings.

- There are two layers: the account layer, which syncs, and the device layer, which never leaves the device.
- One flag per group turns overrides on for that device: `theme` and `display`. The flags are themselves a `device` setting, so they are never synced. Preferences shows each overridable setting with a small "Override on this device" checkbox; when it is checked, a second copy of the control below it edits the device's value.
- While the flag is off, reads return the account's values and writes go to the account.
- While the flag is on, reads return the device's values for every setting that allows overrides, and writes to those settings go to the device. Settings that do not allow overrides keep using the account.
- Turning the flag on copies the current account values into the device layer, so nothing changes on screen at that moment. A setting not yet used on the page when the flag is turned on follows the account until it is set on this device.
- Turning the flag off discards the device's values, so the account's values apply again.
- Code that uses settings never knows which layer answered. `get`, `set` and `useSetting` work the same with or without overrides.

With only the browser storage provider, both layers live in the same browser, so the flag would mean nothing. Build the store with the two layers from the start, but keep the flag hidden in Preferences until the server provider ships.

The theme and the display preferences allow overrides. Sidebar and list widths only make sense on one screen, so they are `device` settings and never sync.

## In scope now

1. The registry, the store, `useSetting`, and the browser storage provider, with the three persistence modes and the account and device layers (the device flag hidden).
2. Move the `synced` settings onto the store, keeping their keys: `src/lib/theme.tsx`, `src/lib/user-preferences.tsx`, `src/lib/sessions/list-preferences.ts`, `src/lib/universes.ts` (last universe), and `src/lib/welcome.ts`. Their exported hooks can stay as thin wrappers, so their callers do not change.
3. Move the `device` and `tab` settings onto the store, keeping their keys: composer drafts (`src/lib/sessions/draft.ts`), model choices (`src/lib/composer-model.ts`), attachments (`src/lib/composer-attachments.ts`), design token overrides (`src/devtools/design-tokens.ts`), and the debug toolbar (`src/devtools/DebugToolbar.tsx`).
4. The theme follows other tabs.
5. Remove the unused `sidebar_state` cookie write, if the sidebar component can be changed without fighting its upstream copy.

Two uses of browser storage stay outside the store: the inline script in `index.html`, which reads `lightspeed:theme` before any code loads, and the pending flag in `src/lib/automatic-sign-in.ts`, which is a step of the sign-in flow rather than a setting.

## Tests

- A missing, unknown or invalid stored value gives the default; a valid one is kept.
- `set` changes `get` and notifies listeners at once, and resolves after the provider saved.
- With blocked or full storage, `set` rejects, the value stays for the page, and nothing throws during render.
- `synced` and `device` settings are saved to `localStorage`, and `tab` settings to `sessionStorage`.
- A `storage` event from another tab updates every `synced` and `device` setting, the theme included.
- Batched writes: many quick changes to a setting reach storage as one write, and a pending write is saved when the page is hidden.
- Layers: with the flag off, reads and writes use the account layer; with it on, they use the device layer for settings that allow it; turning it on copies the account values. `device` and `tab` settings ignore the flag.
- `index.html` still applies the stored theme before first paint.
- Later, with the server provider: `device` and `tab` settings are never sent to the server.

## Progress

The first stage, everything in scope now, is done. The store, registry, `useSetting` and the browser storage provider live in `src/lib/settings/`, and every setting in the table above is read and written through them.

- Keys and value formats are unchanged, with one exception: sidebar and list widths moved to `lightspeed:layout:<userId>`, because they are `device` settings and every key has one persistence. They are still read from the old display preferences object until they are set again, so nobody loses a width.
- Display preferences are now written one field at a time, so the stored object holds only the fields that were set.
- The device layer keeps each group's values in `lightspeed:device-settings:<group>` and the flags in `lightspeed:device-overrides`. `index.html` reads the device's theme before first paint when the theme flag is on.
- Drafts are written 250 ms after typing stops and design token overrides at most once per frame. Held-back writes are saved when the page is hidden or unloaded.
- Drafts, model choices, attachments and queued steering recovery expire after 30 days without being read or written. Their last use is kept in `lightspeed:settings:last-used`, and expired entries are dropped when the page loads.
- Design token override values now reach other windows through `storage` events. The `BroadcastChannel` stays, because the design editor also uses it to find which app windows are open; it carries only that ping and reply.
- An open composer keeps its own text rather than following another tab's draft live, so the caret never jumps. The saved draft is read again when the composer mounts.
- The unused `sidebar_state` cookie write is removed.

## Open work

- The Preferences UI for the device override checkboxes, hidden until the server provider ships.
- The server provider ([U108](u108-server-saved-settings.md)), and its test that `device` and `tab` settings are never sent to the server.

## Open questions

- Is the device flag one switch for all overridable settings, or one per group (theme, display)?
  - Answer: it's per group
- When a device turns its flag off, are its values discarded, or kept for the next time it is turned on?
  - Answer: set back to the synced value: the UI works like this: it has the setting toggle/value/input and a small checkbox override on this device. if it is enabled, then an additional copy of the toggle/value/input is shown below it
- Which settings are `device` rather than `synced` (sidebar and list widths are candidates)?
  - Answer: sidebar and list widths yes
- Drafts, model choices and attachments of deleted sessions stay in storage. Should the store drop `device` entries whose session no longer exists, or expire them after some time?
  - Answer: yes they should expire
- Can `storage` events replace the `BroadcastChannel` for design token overrides, or does dragging a slider need the channel's speed?
  - Answer: yes if BroadcastChannel is only used for design toekn overrides, then yes it can (and should be replaced)
