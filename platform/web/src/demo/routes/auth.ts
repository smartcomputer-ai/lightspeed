/// better-auth endpoints the web client calls. The demo has one signed-in
/// platform admin and never asks for credentials.
import { Hono } from "hono";
import type { DemoStore, DemoUser } from "../store";
import { nowIso, readBody } from "./common";

export function authRoutes(store: DemoStore): Hono {
  const app = new Hono();

  const sessionOf = (user: DemoUser) => ({
    session: {
      id: "demo-session",
      userId: user.id,
      token: "demo-token",
      expiresAt: new Date(Date.now() + 30 * 86_400_000).toISOString(),
      createdAt: user.createdAt,
      updatedAt: nowIso(),
      ipAddress: "",
      userAgent: "",
      activeOrganizationId: null,
    },
    user,
  });

  app.get("/get-session", (c) => c.json(sessionOf(store.currentUser)));
  app.post("/sign-in/email", (c) =>
    c.json({ redirect: false, token: "demo-token", user: store.currentUser }),
  );
  app.post("/sign-out", (c) => c.json({ success: true }));
  app.post("/update-user", async (c) => {
    const body = await readBody<{ name?: string; image?: string | null }>(c);
    if (typeof body.name === "string" && body.name.trim()) store.currentUser.name = body.name.trim();
    if (body.image !== undefined) store.currentUser.image = body.image;
    store.currentUser.updatedAt = nowIso();
    return c.json({ status: true });
  });
  app.post("/change-password", (c) => c.json({ token: "demo-token", user: store.currentUser }));

  return app;
}
