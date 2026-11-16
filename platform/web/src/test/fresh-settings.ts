import { beforeEach } from "vitest";
import { settings } from "@/lib/settings";

// Each test starts on a fresh page: no setting write held back for later
// and no value kept in memory after a failed save, from an earlier test.
beforeEach(() => {
  settings.discardUnsaved();
});
