/// Reads the icon modules and the app's source for the UI overview: which
/// roles the contract in src/icons/roles.ts declares, the icon a library
/// draws each with, and which files import each role. It works on raw
/// source text, so the overview follows the code without upkeep.

export interface IconRole {
  /// The name components import, such as `DeleteIcon`.
  role: string;
  /// The `// Heading.` comment the role sits under in roles.ts.
  section: string;
}

export interface RoleGlyph {
  /// The icon's name in its library, such as `Trash2`.
  glyph: string;
  /// The module it comes from, such as `lucide-react`.
  library: string;
}

/// Every role the `IconSet` interface declares, in order, under its section.
export function iconRoles(rolesSource: string): IconRole[] {
  const body = /export interface IconSet \{([\s\S]*?)\n\}/.exec(rolesSource)?.[1] ?? "";
  const roles: IconRole[] = [];
  let section = "Icons";
  for (const line of body.split("\n")) {
    const heading = /^\s*\/\/ (.+?)\.?$/.exec(line);
    if (heading) section = heading[1]!;
    const member = /^\s*(\w+)\s*:/.exec(line);
    if (member) roles.push({ role: member[1]!, section });
  }
  return roles;
}

/// The icon a library module draws each role with. Entries look like
/// `DeleteIcon: Trash2,` or `DeleteIcon: wrap(Trash),`; the glyph's library
/// is whichever import brings it in.
export function libraryGlyphs(librarySource: string): Map<string, RoleGlyph> {
  const origins = new Map<string, string>();
  for (const [, typeOnly, list, from] of librarySource.matchAll(/import\s+(type\s+)?\{([^}]*)\}\s*from\s*["']([^"']+)["']/g)) {
    if (typeOnly) continue;
    for (const entry of list!.split(",")) {
      const name = entry.trim().split(/\s+as\s+/).at(-1)!.trim();
      if (name && !name.startsWith("type ")) origins.set(name, from!);
    }
  }
  const glyphs = new Map<string, RoleGlyph>();
  for (const [, role, glyph] of librarySource.matchAll(/^\s*(\w+(?:Icon|Logo)):\s*(?:\w+\()?(\w+)\)?,/gm)) {
    glyphs.set(role!, { glyph: glyph!, library: origins.get(glyph!) ?? "local" });
  }
  return glyphs;
}

/// The role names index.ts destructures and exports.
export function exportedRoles(indexSource: string): string[] {
  const list = /export const \{([^}]*)\}\s*=\s*icons;/.exec(indexSource)?.[1] ?? "";
  return list.split(",").map((name) => name.trim()).filter(Boolean);
}

const IMPORT_PATTERN = /import\s+(type\s+)?\{([^}]*)\}\s*from\s*["']@\/icons["']/g;

/// The role names one module imports from `@/icons`; type-only imports are
/// skipped.
export function roleImports(source: string): string[] {
  const names: string[] = [];
  for (const [, typeOnly, list] of source.matchAll(IMPORT_PATTERN)) {
    if (typeOnly) continue;
    for (const entry of list!.split(",")) {
      const text = entry.trim();
      if (!text || text.startsWith("type ")) continue;
      names.push(text.split(/\s+as\s+/)[0]!.trim());
    }
  }
  return names;
}

/// For each role, the files that import it (`src/...`, sorted).
export function collectRoleUsage(sources: Record<string, string>): Map<string, string[]> {
  const usage = new Map<string, string[]>();
  for (const [path, source] of Object.entries(sources)) {
    const file = path.replace(/^\//, "");
    for (const role of new Set(roleImports(source))) usage.set(role, [...(usage.get(role) ?? []), file]);
  }
  for (const files of usage.values()) files.sort();
  return usage;
}
