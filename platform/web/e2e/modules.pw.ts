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
  await expect(page.getByRole("link", { name: "weekly-review.md" })).toBeVisible();
  await page.getByLabel("Files to publish").setInputFiles({
    name: "release-summary.md",
    mimeType: "text/markdown",
    buffer: Buffer.from("# Ready for review"),
  });
  await page.getByRole("button", { name: "Publish", exact: true }).click();
  await page.getByRole("link", { name: "release-summary.md" }).click();
  await expect(page).toHaveURL(/\/blobs\/[\da-f]{64}\?/);
  await expect(page.getByRole("heading", { name: "release-summary.md" })).toBeVisible();
  await page.goBack();
  await expect(page.getByRole("link", { name: "release-summary.md" })).toBeVisible();

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
  await expect(page.getByLabel("Files to publish")).toHaveCount(0);

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
  await expect(page.getByRole("link", { name: "release-summary.md" })).toBeVisible();
});

test("Bots keeps its shared session view when the Sessions module is disabled", async ({ page }) => {
  await page.goto("u/software-factory/bots/planner");
  const composer = page.getByRole("textbox", { name: "Message", exact: true });
  await expect(composer).toBeVisible({ timeout: 30_000 });
  await page.getByRole("link", { name: "Sessions", exact: true }).click();
  await page.getByRole("link", { name: /^Fix flaky scheduler test/ }).click();
  await expect(page).toHaveURL(/\/sessions\/session-flaky-scheduler$/);
  await expect(composer).toBeVisible();
  await page.goBack();
  await page.goBack();
  await expect(page).toHaveURL(/\/bots\/planner$/);

  await openOrganizationSettings(page);
  const enabled = page.getByRole("switch", { name: "Sessions" });
  await expect(enabled).toBeChecked();
  await enabled.click();
  await expect(enabled).not.toBeChecked();
  await page.goBack();
  await page.goBack();
  await expect(page).toHaveURL(/\/bots\/planner$/);
  await expect(page.getByRole("link", { name: "Sessions", exact: true })).toHaveCount(0);
  await expect(composer).toBeVisible();
});

async function openOrganizationSettings(page: Page) {
  await page.getByRole("button", { name: /Ada Demo/ }).click();
  await page.getByRole("menuitem", { name: "Admin", exact: true }).click();
  await page.locator('a[href$="/demo-organization/settings"]').click();
  await expect(page.getByRole("heading", { name: "Organization settings" })).toBeVisible();
}
