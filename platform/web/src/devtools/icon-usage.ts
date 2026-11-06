/// Finds which icons the app imports and from which files, by reading the
/// import statements of its source. The UI overview feeds it the raw source
/// of every module, so the list follows the code without upkeep.

export type IconLibrary = "lucide" | "tabler" | "local";

/// Modules that export icon components, and the library each belongs to.
export const ICON_MODULES: Record<string, IconLibrary> = {
  "lucide-react": "lucide",
  "@tabler/icons-react": "tabler",
  "@/components/icons/bot": "local",
  "@/components/icons/logos": "local",
};

export interface IconImport {
  name: string;
  library: IconLibrary;
}

export interface IconUsage extends IconImport {
  /// Source files that import the icon, as `src/...` paths, sorted.
  files: string[];
}

const IMPORT_PATTERN = /import\s+(type\s+)?\{([^}]*)\}\s*from\s*["']([^"']+)["']/g;

/// The icon components one module imports: type-only names are skipped and
/// an aliased import (`Globe as GlobeIcon`) counts under its exported name.
export function iconImports(source: string): IconImport[] {
  const imports: IconImport[] = [];
  for (const [, typeOnly, names, from] of source.matchAll(IMPORT_PATTERN)) {
    const library = ICON_MODULES[from!];
    if (!library || typeOnly) continue;
    for (const entry of names!.split(",")) {
      const text = entry.trim();
      if (!text || text.startsWith("type ")) continue;
      const name = text.split(/\s+as\s+/)[0]!.trim();
      if (/^[A-Z]\w*$/.test(name)) imports.push({ name, library });
    }
  }
  return imports;
}

/// Every imported icon with the files that use it, most used first.
export function collectIconUsage(sources: Record<string, string>): IconUsage[] {
  const usage = new Map<string, IconUsage>();
  for (const [path, source] of Object.entries(sources)) {
    const file = path.replace(/^\//, "");
    for (const icon of iconImports(source)) {
      const key = `${icon.library}:${icon.name}`;
      const entry = usage.get(key) ?? { ...icon, files: [] };
      if (!entry.files.includes(file)) entry.files.push(file);
      usage.set(key, entry);
    }
  }
  return [...usage.values()]
    .map((entry) => ({ ...entry, files: entry.files.sort() }))
    .sort((a, b) => b.files.length - a.files.length || a.name.localeCompare(b.name));
}
