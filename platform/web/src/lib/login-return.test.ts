import { expect, it } from "vitest";
import { appReturnUrl, deviceLoginPath, loginReturnPath } from "./login-return";
it("preserves the device code through authentication without approving it", () => {
  const login = deviceLoginPath("?user_code=ABCD-EFGH", true);
  expect(loginReturnPath(new URL(login, "https://platform.test").search)).toBe("/device?user_code=ABCD-EFGH");
  expect(appReturnUrl(new URL(login, "https://platform.test").search, "https://platform.test", "/app/")).toBe("https://platform.test/app/device?user_code=ABCD-EFGH");
});
it.each(["https://attacker.test/device", "//attacker.test/device", "/device/other", "/deviceevil", "/api/auth/admin", "/device\\\\attacker.test"])("does not redirect authentication to %s", target => {
  expect(loginReturnPath(`?${new URLSearchParams({ returnTo: target })}`)).toBe("/");
});
