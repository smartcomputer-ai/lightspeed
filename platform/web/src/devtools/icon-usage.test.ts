import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { describe, expect, it } from "vitest";
import * as Icons from "@/icons";
import { collectRoleUsage, exportedRoles, iconRoles, libraryGlyphs, roleImports } from "./icon-usage";

const SRC = path.resolve(import.meta.dirname, "..");
const ICONS = path.join(SRC, "icons");
const read = (file: string) => readFileSync(file, "utf8");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const full = path.join(dir, name);
    return statSync(full).isDirectory() ? sourceFiles(full) : /\.(ts|tsx)$/.test(name) ? [full] : [];
  });
}

const libraryFiles = readdirSync(ICONS)
  .filter((name) => /\.(ts|tsx)$/.test(name) && !/^(roles|index)\.ts$/.test(name))
  .map((name) => path.join(ICONS, name));

describe("icon roles", () => {
  it("reads the roles and their sections from the contract", () => {
    const source = `
export interface IconSet {
  // Actions.
  AddIcon: IconComponent;
  DeleteIcon: IconComponent;

  // Controls.
  DialogCloseIcon: IconComponent;
}`;
    expect(iconRoles(source)).toEqual([
      { role: "AddIcon", section: "Actions" },
      { role: "DeleteIcon", section: "Actions" },
      { role: "DialogCloseIcon", section: "Controls" },
    ]);
  });

  it("reads the icon a library draws each role with", () => {
    const source = `
import { Plus, Trash as TrashGlyph } from "some-icons";
import { IconX } from "@tabler/icons-react";
import type { IconLibrary } from "./roles";
export const set = { icons: {
  AddIcon: Plus,
  DeleteIcon: wrap(TrashGlyph),
  DialogCloseIcon: IconX,
} };`;
    expect(Object.fromEntries(libraryGlyphs(source))).toEqual({
      AddIcon: { glyph: "Plus", library: "some-icons" },
      DeleteIcon: { glyph: "TrashGlyph", library: "some-icons" },
      DialogCloseIcon: { glyph: "IconX", library: "@tabler/icons-react" },
    });
  });

  it("lists the files that import each role", () => {
    expect(roleImports(`import { AddIcon, type IconComponent } from "@/icons"; import type { X } from "@/icons";`)).toEqual(["AddIcon"]);
    const usage = collectRoleUsage({
      "/src/b.tsx": `import { AddIcon } from "@/icons";`,
      "/src/a.tsx": `import {\n  AddIcon,\n  DeleteIcon,\n} from "@/icons";`,
    });
    expect(Object.fromEntries(usage)).toEqual({ AddIcon: ["src/a.tsx", "src/b.tsx"], DeleteIcon: ["src/a.tsx"] });
  });

  it("keeps icon libraries behind src/icons", () => {
    // Components import roles from @/icons, so a concept keeps one icon and
    // the library can be swapped in one place.
    const libraries = /from\s*["'](lucide-react|@tabler\/icons-react|@phosphor-icons\/[\w-]+|@\/components\/icons\/[\w-]+)["']/;
    const offenders = sourceFiles(SRC)
      .filter((file) => !file.startsWith(ICONS + path.sep) && !/\.test\.tsx?$/.test(file))
      .filter((file) => libraries.test(read(file)))
      .map((file) => path.relative(SRC, file));
    expect(offenders).toEqual([]);
  });
});

describe("icon contract", () => {
  const roles = iconRoles(read(path.join(ICONS, "roles.ts"))).map((entry) => entry.role);

  it("declares every role once", () => {
    expect(roles.length).toBeGreaterThan(100);
    expect(roles.filter((role, index) => roles.indexOf(role) !== index)).toEqual([]);
  });

  it("exports exactly the declared roles from @/icons", () => {
    expect(exportedRoles(read(path.join(ICONS, "index.ts")))).toEqual(roles);
    const exported = Icons as unknown as Record<string, unknown>;
    expect(roles.filter((role) => typeof exported[role] !== "function")).toEqual([]);
  });

  it.each(libraryFiles.map((file) => [path.basename(file), file]))("%s draws every role", (_name, file) => {
    // The typecheck already enforces the IconSet shape; this keeps the
    // overview's glyph listing in step with it.
    const glyphs = libraryGlyphs(read(file));
    expect(roles.filter((role) => !glyphs.has(role))).toEqual([]);
  });
});
