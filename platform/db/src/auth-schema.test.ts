import { getAuthTables } from "better-auth/db";
import { getTableColumns, type Table } from "drizzle-orm";
import { expect, it } from "vitest";
import { auth } from "../scripts/auth-codegen.js";
import * as schema from "./schema/index.js";
import { deviceCodeFields, identitySessionFields, identityUserFields, organizationFields } from "./auth-fields.js";

const tables = getAuthTables(auth.options);
const lightspeedFields: Record<string, object> = {
  user: identityUserFields, session: identitySessionFields, organization: organizationFields, deviceCode: deviceCodeFields,
};

/// Every column on a Better Auth table is either Better Auth's own or one of
/// Lightspeed's, declared in auth-fields.ts. A Better Auth upgrade that adds,
/// renames or drops a column fails here until the schema and migration follow.
it.each(Object.keys(tables))("%s matches Better Auth's schema plus Lightspeed's declared columns", (key) => {
  const table = (schema as unknown as Record<string, Table>)[key];
  expect(table, `no Drizzle table for Better Auth's ${key}`).toBeDefined();
  const expected = ["id", ...Object.keys(tables[key]!.fields)].sort();
  expect(Object.keys(getTableColumns(table!)).sort()).toEqual(expected);
});

it("declares Lightspeed's columns only on tables Better Auth knows", () => {
  for (const [key, fields] of Object.entries(lightspeedFields)) {
    expect(Object.keys(tables[key]?.fields ?? {})).toEqual(expect.arrayContaining(Object.keys(fields)));
  }
});
