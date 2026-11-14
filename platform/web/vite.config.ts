import { readFileSync } from "node:fs";
import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig, type Plugin } from "vite";

// The SPA lives under /app on the platform origin (path-based routing).
// In dev, vite proxies API calls to the local server so the better-auth
// cookie flow works without CORS.
//
// `--mode demo` is the second build path: the same app with the in-browser
// backend (src/demo) swapped in as the entry, no proxy, and its own output
// directory, so it can be published as a static site.
export default defineConfig(({ mode }) => {
  const demo = mode === "demo";
  return {
    base: demo ? "/demo/" : "/app/",
    plugins: [react(), tailwindcss(), todoGuard(), ...(demo ? [demoEntry()] : [])],
    resolve: {
      alias: {
        "@": path.resolve(import.meta.dirname, "./src"),
      },
    },
    build: demo ? { outDir: "dist-demo" } : undefined,
    server: {
      port: demo ? 5175 : 5173,
      // Fail instead of hopping to 5174: only :5173 is a trusted auth origin,
      // so a silently shifted port would break sign-in.
      strictPort: true,
      proxy: demo
        ? undefined
        : {
            "/api": "http://localhost:3000",
          },
    },
  };
});

/// Points index.html at the demo entry, which installs the in-browser
/// backend before loading the real app.
function demoEntry(): Plugin {
  return {
    name: "lightspeed-demo-entry",
    transformIndexHtml: {
      order: "pre",
      handler: (html) => html.replace('src="/src/main.tsx"', 'src="/src/demo/main.ts"'),
    },
  };
}

/// The `<Todo>` placeholder (src/components/todo.tsx) must not ship: the
/// production build fails while it is in the output, naming each to-do with
/// its file, line, title and description. Development, tests and the demo
/// build are not checked. Only what is rendered into the bundle counts, so
/// a to-do in code the build drops (development-only pages) does not.
function todoGuard(): Plugin {
  const todoModule = path.resolve(import.meta.dirname, "src/components/todo.tsx");
  const root = import.meta.dirname;
  const usages = new Map<string, Array<{ line: number | null; title: string; description: string | null }>>();
  const same = (left: string, right: string) => path.normalize(left).toLowerCase() === path.normalize(right).toLowerCase();
  const relative = (id: string) => path.relative(root, id).split(path.sep).join("/");
  return {
    name: "lightspeed-todo-guard",
    apply: (_config, env) => env.command === "build" && env.mode === "production",
    async transform(code, id) {
      const file = id.split("?")[0]!;
      if (file.includes("node_modules") || same(file, todoModule) || !/todo["']/i.test(code)) return null;
      const program = this.parse(code) as unknown as { body: AstNode[] };
      // The names this module gives the Todo component.
      const names = new Set<string>();
      for (const node of program.body) {
        if (node.type !== "ImportDeclaration") continue;
        const resolved = await this.resolve(node.source.value, id);
        if (!resolved || !same(resolved.id.split("?")[0]!, todoModule)) continue;
        for (const specifier of node.specifiers) {
          if (specifier.type === "ImportSpecifier" && (specifier.imported.name ?? specifier.imported.value) === "Todo") {
            names.add(specifier.local.name);
          }
        }
      }
      if (names.size === 0) return null;
      // Each <Todo> is now a jsx(Todo, { ...props }) call; read its text props.
      const found: Array<{ title: string; description: string | null }> = [];
      const text = (node: AstNode | undefined): string | null => {
        if (!node) return null;
        if (node.type === "Literal" && typeof node.value === "string") return node.value;
        if (node.type === "TemplateLiteral" && node.expressions.length === 0) return node.quasis.map((quasi: AstNode) => quasi.value.cooked).join("");
        return code.slice(node.start, node.end);
      };
      const visit = (node: unknown): void => {
        if (!node || typeof node !== "object") return;
        if (Array.isArray(node)) return node.forEach(visit);
        const current = node as AstNode;
        if (current.type === "CallExpression" && current.arguments[0]?.type === "Identifier" && names.has(current.arguments[0].name)) {
          const props: AstNode[] = current.arguments[1]?.type === "ObjectExpression" ? current.arguments[1].properties : [];
          const prop = (key: string) => props.find((property) => property.type === "Property" && (property.key.name ?? property.key.value) === key)?.value;
          found.push({ title: text(prop("title")) ?? "To Do", description: text(prop("description")) });
        }
        for (const key of Object.keys(current)) if (key !== "type") visit(current[key]);
      };
      visit(program.body);
      // Lines from the source, in the same order as the calls.
      let lines: number[] = [];
      try {
        const source = readFileSync(file, "utf8").split(/\r?\n/);
        const opening = new RegExp(String.raw`<(${[...names].join("|")})\b`);
        lines = source.flatMap((line, index) => (opening.test(line) ? [index + 1] : []));
      } catch {
        // No source to read: report without lines.
      }
      usages.set(file, (found.length > 0 ? found : [{ title: "To Do", description: null }]).map((usage, index) => ({
        line: lines[index] ?? null,
        ...usage,
      })));
      return null;
    },
    generateBundle(_options, bundle) {
      const rendered = new Set<string>();
      for (const output of Object.values(bundle)) {
        if (output.type !== "chunk") continue;
        for (const [id, info] of Object.entries(output.modules)) {
          if (info.renderedLength > 0) rendered.add(path.normalize(id.split("?")[0]!).toLowerCase());
        }
      }
      if (!rendered.has(path.normalize(todoModule).toLowerCase())) return;
      const entries = [...usages]
        .filter(([file]) => rendered.has(path.normalize(file).toLowerCase()))
        .flatMap(([file, list]) => list.map((usage) => ({ file: relative(file), ...usage })))
        .sort((left, right) => left.file.localeCompare(right.file) || (left.line ?? 0) - (right.line ?? 0));
      const list = entries.length > 0
        ? entries.map(({ file, line, title, description }) =>
            `  ${file}${line ? `:${line}` : ""}\n    ${title}${description ? `: ${description}` : ""}`).join("\n")
        : "  (the component is in the bundle, but no <Todo> use was found; search for imports of components/todo)";
      const count = entries.length === 1 ? "1 to-do is" : `${entries.length || "Some"} to-dos are`;
      this.error(
        `Production build failed: ${count} still in the app.\n` +
        `<Todo> (${relative(todoModule)}) marks unfinished work and may not ship in a production build. ` +
        `Finish each one and remove its <Todo>:\n\n${list}\n`,
      );
    },
  };
}

// The few ESTree fields todoGuard reads.
type AstNode = any;
