#!/usr/bin/env node
/// Visual diffs of the demo build.
///
///   screenshots:demo [--theme <light|dark>[,...]]
///     Builds the working tree's demo and captures it into
///     test-results/screenshots/.
///
///   screendiff:demo [base] [head] [--theme <light|dark>[,...]] [--no-cache] [--keep-screenshots]
///     Captures two versions and compares them with screendiff-js into
///     test-results/screendiff/<base>_vs_<head>/, replacing the previous run
///     of the same comparison. The self-contained report.html and
///     results.json remain; the captured PNGs are removed unless
///     --keep-screenshots is given or the run failed. `base` defaults to where the branch split off
///     main (`git merge-base HEAD main`), not main's tip, so changes merged to
///     main since then stay out of the report. `head` defaults to the working
///     tree, uncommitted changes included.
///
/// Committed refs are built in a git worktree in the OS temp folder and the
/// build is cached by commit SHA in node_modules/.cache/. The worktree stays
/// outside the repository on purpose: inside it, module resolution would walk
/// up into this checkout's node_modules and could quietly build an old commit
/// against current packages.
///
/// The capture harness and screen list always come from the working tree, so
/// both sides capture the same screens.
import { execFileSync, spawn } from "node:child_process";
import { cpSync, createReadStream, existsSync, mkdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { createRequire } from "node:module";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const webDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = git(["rev-parse", "--show-toplevel"], webDir);
const resultsDir = path.join(webDir, "test-results");
const buildCache = path.join(webDir, "node_modules", ".cache", "lightspeed-visual");
const worktreeRoot = path.join(tmpdir(), "lightspeed-visual");
/// Fixed port from the dev-port tool for this repository.
const PORT = 40938;
const BASE_PATH = "/demo/";
const THEMES = ["light", "dark"];
const requireFromWeb = createRequire(path.join(webDir, "package.json"));
/// Set by npm while a script runs, so a worktree uses the same npm.
const npmCli = process.env.npm_execpath;

const [command, ...rest] = process.argv.slice(2);
let options;

try {
  options = parseOptions(rest);
  if (command === "capture") process.exitCode = await captureCommand();
  else if (command === "diff") process.exitCode = await diffCommand();
  else fail(`usage: screendiff.mjs capture|diff [base] [head] [--theme light,dark] [--no-cache] [--keep-screenshots]`);
} catch (error) {
  console.error(`\n${error instanceof Error ? error.message : String(error)}`);
  process.exitCode = 2;
}

async function captureCommand() {
  if (options.refs.length > 0) fail("screenshots:demo captures the working tree; use screendiff:demo to compare refs.");
  const dist = buildWorkingTree();
  const out = path.join(resultsDir, "screenshots");
  const failed = await capture(dist, out);
  console.log(`\nScreenshots: ${out}`);
  return failed ? 1 : 0;
}

async function diffCommand() {
  if (options.refs.length > 2) fail("screendiff:demo takes at most two refs: [base] [head].");
  const [baseRef = mergeBase(), headRef] = options.refs;
  const base = { label: shortRef(baseRef), dist: buildRef(baseRef) };
  const head = headRef ? { label: shortRef(headRef), dist: buildRef(headRef) } : { label: "working-tree", dist: buildWorkingTree() };

  // One folder per comparison: running the same comparison again replaces it.
  const run = path.join(resultsDir, "screendiff", `${safe(base.label)}_vs_${safe(head.label)}`);
  rmSync(run, { recursive: true, force: true });
  const failedBase = await capture(base.dist, path.join(run, "base"));
  const failedHead = await capture(head.dist, path.join(run, "head"));

  console.log(`\nComparing ${base.label} → ${head.label}`);
  const report = path.join(run, "report.html");
  const code = await exec([
    ...bin("screendiff-js", "screendiff"),
    "compare", path.join(run, "base"), path.join(run, "head"),
    "--output", path.join(run, "results.json"),
    "--report", report,
    "--title", `Lightspeed demo: ${base.label} → ${head.label}`,
  ], { cwd: webDir, allowFailure: true });
  const failed = code === 2 || failedBase || failedHead;
  // The report embeds both images of every screen, so the PNGs are copies
  // once it exists. A failed run keeps them for a look at what went wrong.
  if (!failed && !options.keepScreenshots && existsSync(report)) {
    rmSync(path.join(run, "base"), { recursive: true, force: true });
    rmSync(path.join(run, "head"), { recursive: true, force: true });
  }
  console.log(`\nReport: ${report}`);
  if (failedBase || failedHead) {
    console.log("Some screens could not be captured (see above); they show as added or removed.");
  }
  return failed ? 2 : code;
}

/// The working tree is rebuilt every time, into the cache so `dist-demo/`
/// is left alone.
function buildWorkingTree() {
  const dist = path.join(buildCache, "working-tree");
  console.log("Building the working tree's demo…");
  ensureSdkBuilt(repoRoot);
  run([...bin("vite"), "build", "--mode", "demo", "--outDir", dist, "--emptyOutDir", "--logLevel", "warn"], { cwd: webDir });
  return dist;
}

/// A commit never changes, so its build is cached by SHA.
function buildRef(ref) {
  const sha = git(["rev-parse", "--verify", `${ref}^{commit}`], repoRoot);
  const dist = path.join(buildCache, sha);
  if (options.cache && existsSync(path.join(dist, "index.html"))) {
    console.log(`Using cached demo build of ${describe(ref, sha)}.`);
    return dist;
  }
  const worktree = path.join(worktreeRoot, sha);
  console.log(`Building the demo of ${describe(ref, sha)} in a temporary worktree: ${worktree}`);
  removeStaleWorktrees();
  removeWorktree(worktree);
  git(["worktree", "add", "--detach", worktree, sha], repoRoot);
  try {
    run(npm(["ci", "--no-audit", "--no-fund", "--loglevel", "error"]), { cwd: worktree });
    ensureSdkBuilt(worktree);
    run(npm(["run", "build:demo", "--workspace", "@lightspeed-ai/platform-web"]), { cwd: worktree });
    rmSync(dist, { recursive: true, force: true });
    cpSync(path.join(worktree, "platform", "web", "dist-demo"), dist, { recursive: true });
  } finally {
    removeWorktree(worktree);
  }
  return dist;
}

function ensureSdkBuilt(root) {
  if (existsSync(path.join(root, "clients", "typescript", "dist", "index.js"))) return;
  run(npm(["run", "build", "--workspace", "@lightspeed-ai/sdk"]), { cwd: root });
}

/// Worktrees of this repository left in the temp folder by an interrupted run.
function removeStaleWorktrees() {
  const listed = git(["worktree", "list", "--porcelain"], repoRoot)
    .split("\n")
    .filter((line) => line.startsWith("worktree "))
    .map((line) => path.resolve(line.slice("worktree ".length)));
  for (const worktree of listed) {
    if (path.dirname(worktree) === path.resolve(worktreeRoot)) removeWorktree(worktree);
  }
}

function removeWorktree(worktree) {
  if (existsSync(worktree)) {
    try {
      git(["worktree", "remove", "--force", worktree], repoRoot);
    } catch {
      rmSync(worktree, { recursive: true, force: true });
    }
  }
  git(["worktree", "prune"], repoRoot);
}

/// Serves one build and runs the Playwright harness against it. Resolves
/// whether any screen failed to capture.
async function capture(dist, out) {
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out, { recursive: true });
  const server = await serve(dist);
  try {
    console.log(`Capturing ${path.relative(repoRoot, out)}…`);
    const code = await exec([...bin("@playwright/test", "playwright"), "test", "-c", "playwright.config.ts"], {
      cwd: webDir,
      allowFailure: true,
      env: {
        SCREENSHOTS_BASE_URL: `http://127.0.0.1:${PORT}${BASE_PATH}`,
        SCREENSHOTS_OUT: out,
        SCREENSHOTS_THEMES: options.themes.join(","),
      },
    });
    return code !== 0;
  } finally {
    await new Promise((resolve) => server.close(resolve));
  }
}

/// Static server for a built demo, with the SPA fallback to index.html.
function serve(dist) {
  const types = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".png": "image/png", ".ico": "image/x-icon", ".json": "application/json", ".woff2": "font/woff2", ".woff": "font/woff", ".webmanifest": "application/manifest+json" };
  const server = createServer((request, response) => {
    const url = new URL(request.url ?? "/", "http://localhost");
    if (!url.pathname.startsWith(BASE_PATH)) {
      response.writeHead(302, { location: BASE_PATH }).end();
      return;
    }
    let file = path.join(dist, decodeURIComponent(url.pathname.slice(BASE_PATH.length)));
    if (!file.startsWith(dist) || !existsSync(file) || statSync(file).isDirectory()) file = path.join(dist, "index.html");
    response.writeHead(200, { "content-type": types[path.extname(file)] ?? "application/octet-stream" });
    createReadStream(file).pipe(response);
  });
  return new Promise((resolve, reject) => {
    server.once("error", (error) => reject(error.code === "EADDRINUSE" ? new Error(`Port ${PORT} is in use; stop whatever serves it and retry.`) : error));
    server.listen(PORT, "127.0.0.1", () => resolve(server));
  });
}

function mergeBase() {
  for (const main of ["main", "origin/main"]) {
    try {
      return git(["merge-base", "HEAD", main], repoRoot);
    } catch {
      // Try the next candidate.
    }
  }
  fail("Could not find main or origin/main to compare against; pass a base ref.");
}

function shortRef(ref) {
  return /^[0-9a-f]{40}$/.test(ref) ? ref.slice(0, 10) : ref;
}

function parseOptions(args) {
  const parsed = { refs: [], themes: THEMES, cache: true, keepScreenshots: false };
  for (let index = 0; index < args.length; index++) {
    const arg = args[index];
    if (arg === "--theme" || arg.startsWith("--theme=")) {
      const value = arg.includes("=") ? arg.slice(arg.indexOf("=") + 1) : args[++index];
      const themes = (value ?? "").split(",").filter(Boolean);
      if (themes.length === 0 || themes.some((theme) => !THEMES.includes(theme))) fail(`--theme takes ${THEMES.join(", ")}, comma-separated.`);
      parsed.themes = themes;
    } else if (arg === "--no-cache") {
      parsed.cache = false;
    } else if (arg === "--keep-screenshots") {
      parsed.keepScreenshots = true;
    } else if (arg.startsWith("-")) {
      fail(`Unknown option ${arg}.`);
    } else {
      parsed.refs.push(arg);
    }
  }
  return parsed;
}

/// A package's CLI, run with this Node. Resolving it the way Node does finds
/// it wherever npm hoisted it, and needs no shell.
function bin(pkg, name = pkg) {
  // Not `resolve("<pkg>/package.json")`: packages with an exports map may not export it.
  const manifestPath = (requireFromWeb.resolve.paths(pkg) ?? [])
    .map((dir) => path.join(dir, pkg, "package.json"))
    .find((candidate) => existsSync(candidate));
  if (!manifestPath) fail(`${pkg} is not installed; run npm install.`);
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  const entry = typeof manifest.bin === "string" ? manifest.bin : manifest.bin[name];
  return [process.execPath, path.join(path.dirname(manifestPath), entry)];
}

function npm(args) {
  return npmCli ? [process.execPath, npmCli, ...args] : ["npm", ...args];
}

function git(args, cwd) {
  return execFileSync("git", args, { cwd, encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] }).trim();
}

/// Commands are `[executable, ...args]`. Only a bare `npm` (no npm_execpath)
/// needs a shell on Windows, where it is a .cmd file.
function run([command, ...args], { cwd }) {
  execFileSync(command, args, { cwd, stdio: "inherit", shell: command === "npm" && process.platform === "win32" });
}

function exec([command, ...args], { cwd, env = {}, allowFailure = false }) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd, stdio: "inherit", env: { ...process.env, ...env } });
    child.once("error", reject);
    child.once("exit", (code) => {
      if (code !== 0 && !allowFailure) reject(new Error(`${path.basename(args[0] ?? command)} exited with ${code}`));
      else resolve(code ?? 1);
    });
  });
}

function safe(label) {
  return label.replace(/[^A-Za-z0-9._-]+/g, "-");
}

function fail(message) {
  throw new Error(message);
}

/// `main (d502672d1d)`, or just the short SHA when the ref already is one.
function describe(ref, sha) {
  return sha.startsWith(ref) ? sha.slice(0, 10) : `${ref} (${sha.slice(0, 10)})`;
}
