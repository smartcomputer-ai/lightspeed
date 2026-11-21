import { expect, test, type Page } from "@playwright/test";

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("lightspeed:welcome-dismissed", "true"));
});

// The demo's org settings and VFS live in the running page. Use browser history
// to revisit a deep link after the admin changes the organization's installation.
test("a workspace publishes an artifact and its organization controls both contributions", async ({
  page,
}) => {
  await page.goto("u/personal-assistant/workspaces/briefs");
  await expect(page.getByRole("link", { name: "Artifacts", exact: true })).toBeVisible({ timeout: 30_000 });
  await page.getByRole("button", { name: "Workspace actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "View artifacts" }).click();

  await expect(page).toHaveURL(/\/artifacts\?workspace=briefs$/);
  await expect(page.getByText("No published artifacts")).toBeVisible();
  await page.getByRole("button", { name: "Publish artifact", exact: true }).click();
  await page.getByRole("combobox", { name: "Workspace file" }).click();
  await page.getByRole("option", { name: /^week-\d+-review\.md$/ }).click();
  await page.getByLabel("Title (optional)").fill("Friday review");
  await page.getByRole("button", { name: "Publish", exact: true }).click();
  const published = page.getByRole("link", { name: "Friday review", exact: true });
  await published.click();
  await expect(page).toHaveURL(/\/artifacts\/[\da-f-]{36}$/);
  await expect(page.getByRole("heading", { name: "Friday review" })).toBeVisible();
  await expect(page.locator("pre")).toContainText("review");
  await page.goBack();
  await expect(published).toBeVisible();

  await openOrganizationSettings(page);
  const enabled = page.getByRole("switch", { name: "Artifacts" });
  await expect(enabled).toBeChecked();
  await enabled.click();
  await expect(enabled).not.toBeChecked();
  await page.goBack();
  await page.goBack();
  await expect(page).toHaveURL(/\/artifacts\?workspace=briefs$/);
  await expect(page.getByText("The Artifacts module is unavailable in this universe.")).toBeVisible();
  await expect(page.getByRole("link", { name: "Artifacts", exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Publish artifact" })).toHaveCount(0);

  await page.goBack();
  await page.getByRole("button", { name: "Workspace actions", exact: true }).click();
  await expect(page.getByRole("menuitem", { name: "View artifacts" })).toHaveCount(0);
  await page.keyboard.press("Escape");

  await openOrganizationSettings(page);
  await enabled.click();
  await expect(enabled).toBeChecked();
  await page.goBack();
  await page.goBack();
  await page.getByRole("button", { name: "Workspace actions", exact: true }).click();
  await page.getByRole("menuitem", { name: "View artifacts" }).click();
  await expect(published).toBeVisible();
  await page.getByRole("button", { name: "Retract Friday review" }).click();
  await page.getByRole("button", { name: "Retract artifact", exact: true }).click();
  await expect(published).toHaveCount(0);
  await expect(page.getByText("No published artifacts")).toBeVisible();
});

test("Bots can send messages when Session Browser is disabled", async ({ page }) => {
  await page.goto("u/software-factory/bots/planner");
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await expect(composer).toBeVisible({ timeout: 30_000 });
  await page.getByRole("link", { name: "Session Browser", exact: true }).click();
  await page.getByRole("link", { name: /^Fix flaky scheduler test/ }).click();
  await expect(page).toHaveURL(/\/sessions\/session-flaky-scheduler$/);
  await expect(composer).toBeVisible();
  await page.goBack();
  await page.goBack();
  await expect(page).toHaveURL(/\/bots\/planner$/);

  await openOrganizationSettings(page);
  const enabled = page.getByRole("switch", { name: "Session Browser", exact: true });
  await expect(enabled).toBeChecked();
  await enabled.click();
  await expect(enabled).not.toBeChecked();
  await page.goBack();
  await page.goBack();
  await expect(page).toHaveURL(/\/bots\/planner$/);
  await expect(page.getByRole("link", { name: "Session Browser", exact: true })).toHaveCount(0);
  const message = "Which tasks are ready for review?";
  await composer.fill(message);
  await composer.press("Enter");
  await expect(page.getByText(message, { exact: true })).toBeVisible();
  await page.getByRole("link", { name: "Bots", exact: true }).click();
  await expect(page).toHaveURL(/\/bots$/);
  await page.goBack();
  await expect(page.getByText(message, { exact: true })).toBeVisible();
});

test("disabling Bots removes its routes and controller UI while managed sessions remain readable", async ({ page }) => {
  await page.goto("u/software-factory/sessions/bot:v1:planner");
  const actions = page.getByRole("button", { name: "Session actions" });
  await actions.click();
  const openInBot = page.getByRole("menuitem", { name: "Open in bot" });
  await openInBot.click();
  await expect(page).toHaveURL(/\/bots\/planner\/chat\//);
  await expect(page.getByRole("textbox", { name: "Message", exact: true })).toBeVisible();

  await openOrganizationSettings(page);
  const enabled = page.getByRole("switch", { name: "Bots", exact: true });
  await expect(enabled).toBeChecked();
  await enabled.click();
  await expect(enabled).not.toBeChecked();
  await page.goBack();
  await page.goBack();
  await expect(page.getByText("The Bots module is unavailable in this universe.")).toBeVisible();
  await expect(page.getByRole("link", { name: "Bots", exact: true })).toHaveCount(0);

  await page.goBack();
  await expect(page).toHaveURL(/\/sessions\/bot:v1:planner$/);
  await expect(page.getByRole("switch", { name: "Direct input" })).toBeVisible();
  await expect(page.getByText(/Managed by bot_controller_v1 — flip to message/)).toBeVisible();
  await actions.click();
  await expect(page.getByRole("menuitem", { name: "Session settings" })).toBeVisible();
  await expect(openInBot).toHaveCount(0);
  await page.keyboard.press("Escape");

  await page.getByRole("button", { name: /Software Factory/ }).click();
  await page.getByRole("menuitem", { name: "Software Factory", exact: true }).click();
  await expect(page).toHaveURL(/\/u\/software-factory\/sessions$/);
});

async function openOrganizationSettings(page: Page) {
  await page.getByRole("button", { name: /Ada Demo/ }).click();
  await page.getByRole("menuitem", { name: "Admin", exact: true }).click();
  await page.locator('a[href$="/demo-organization/settings"]').click();
  await expect(page.getByRole("heading", { name: "Organization settings" })).toBeVisible();
}
