/// Platform-wide fixtures: users beside the demo admin, operator-registered
/// environment providers.
import type { DemoStore, DemoUser } from "../store";

export const INCUS_PROVIDER_ID = "incus-eu-1";

const OTHER_USERS: DemoUser[] = [
  {
    id: "user-marco",
    name: "Marco Ruiz",
    email: "marco@acme.example",
    role: "user",
    emailVerified: true,
    image: null,
    banned: false,
    createdAt: "2026-06-03T10:00:00.000Z",
    updatedAt: "2026-08-01T08:30:00.000Z",
  },
  {
    id: "user-priya",
    name: "Priya Natarajan",
    email: "priya@acme.example",
    role: "user",
    emailVerified: true,
    image: null,
    banned: false,
    createdAt: "2026-06-10T15:45:00.000Z",
    updatedAt: "2026-08-12T11:20:00.000Z",
  },
  {
    id: "user-jonas",
    name: "Jonas Lindqvist",
    email: "jonas@northwind.example",
    role: "user",
    emailVerified: true,
    image: null,
    banned: false,
    createdAt: "2026-07-01T09:00:00.000Z",
    updatedAt: "2026-08-18T16:40:00.000Z",
  },
];

export function seedPlatform(store: DemoStore): void {
  for (const user of OTHER_USERS) store.users.set(user.id, { ...user });

  store.environmentProviders.set(INCUS_PROVIDER_ID, {
    providerId: INCUS_PROVIDER_ID,
    displayName: "Incus (eu-1)",
    controllerEndpoint: "wss://incus-eu-1.lightspeed.demo:19090/control",
    effectiveDataRouteBase: "wss://incus-eu-1.lightspeed.demo:19090/routes",
    hasCredential: true,
    // Each demo universe is bound to it.
    availableToAllUniverses: false,
    createdAtMs: Date.parse("2026-06-05T12:00:00.000Z"),
    updatedAtMs: Date.parse("2026-08-10T09:00:00.000Z"),
  });
}
