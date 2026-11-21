import path from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";
import { expect, it } from "vitest";

const web = fileURLToPath(new URL("../../", import.meta.url));
const root = path.resolve(web, "../..");
const host = "platform/web/src/";
const featurePackage = /^modules\/([^/]+)\/(web|components|demo)\//;

function repoPath(file: string) {
  return path.relative(root, file).split(path.sep).join("/");
}

it("keeps the web host, reusable module components, and standalone module entries independently composable", () => {
  const config = ts.readConfigFile(path.join(web, "tsconfig.json"), ts.sys.readFile);
  if (config.error) throw new Error(ts.flattenDiagnosticMessageText(config.error.messageText, "\n"));
  const { options, fileNames } = ts.parseJsonConfigFileContent(config.config, ts.sys, web);
  const cache = ts.createModuleResolutionCache(web, name => name, options);
  const violations: string[] = [];

  for (const file of fileNames) {
    const from = repoPath(file);
    const feature = featurePackage.exec(from);
    if (!from.startsWith(host) && !feature) continue; // Distribution entries assemble the features.
    // Module tests can use the host's test harness. Host tests must run with modules absent.
    if (feature && /\.test\.tsx?$/.test(file)) continue;

    const source = ts.sys.readFile(file)!;
    for (const reference of ts.preProcessFile(source, true, true).importedFiles) {
      const specifier = reference.fileName;
      const resolved = ts.resolveModuleName(specifier, file, options, ts.sys, cache).resolvedModule;
      if (!resolved) continue; // TypeScript reports unresolved source imports in the web typecheck.
      const to = repoPath(resolved.resolvedFileName);
      const target = featurePackage.exec(to);
      let reason: string | undefined;
      if (to.startsWith("distributions/") || (from.startsWith(host) && to.startsWith("modules/"))) {
        reason = "The distribution supplies module contributions to the host";
      } else if (feature && to.startsWith(host) && !specifier.startsWith("@lightspeed-ai/platform-web/")) {
        reason = "Modules consume the public Platform web exports";
      } else if (feature && target?.[2] === "web" && (feature[1] !== target[1] || feature[2] !== "web")) {
        reason = "Shared UI belongs in an importable library separate from the installed feature entry";
      }
      if (reason) {
        const line = source.slice(0, reference.pos).split("\n").length;
        violations.push(`${from}:${line} imports ${specifier} (${to}). ${reason}.`);
      }
    }
  }

  expect(violations).toEqual([]);
});
