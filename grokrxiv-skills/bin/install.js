#!/usr/bin/env node
// @grokrxiv/skills — installer for the grokrxiv-review skill into Claude Code,
// Gemini, and Codex CLIs. Pure Node 18+ stdlib. No npm deps.

import { spawnSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const PACKAGE_VERSION = "0.1.0";
const SKILL_NAME = "grokrxiv-review";
const CODEX_BLOCK_REGEX =
  /\n*<!--\s*BEGIN grokrxiv-skills v[^\s>]+\s*-->[\s\S]*?<!--\s*END grokrxiv-skills v[^\s>]+\s*-->\n*/g;

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const PACKAGE_ROOT = resolve(__dirname, "..");

const HOME = process.env.HOME || homedir();
const CLAUDE_SKILLS_DIR = join(HOME, ".claude", "skills");
const GEMINI_SKILLS_DIR = join(HOME, ".gemini", "skills");
const CODEX_DIR = join(HOME, ".codex");
const CODEX_AGENTS_FILE = join(CODEX_DIR, "AGENTS.md");
const SHARED_SCHEMAS_DIR = join(HOME, ".grokrxiv", "skills", "schemas");

const FLAGS = parseFlags(process.argv.slice(2));

function parseFlags(args) {
  const positional = [];
  const flags = { force: false, dryRun: false, help: false };
  for (const arg of args) {
    if (arg === "--force" || arg === "-f") flags.force = true;
    else if (arg === "--dry-run" || arg === "-n") flags.dryRun = true;
    else if (arg === "--help" || arg === "-h") flags.help = true;
    else positional.push(arg);
  }
  return { ...flags, positional };
}

function log(msg) {
  process.stdout.write(`${msg}\n`);
}

function warn(msg) {
  process.stderr.write(`[warn] ${msg}\n`);
}

function err(msg) {
  process.stderr.write(`[error] ${msg}\n`);
}

function which(cmd) {
  const r = spawnSync("which", [cmd], { encoding: "utf8" });
  if (r.status === 0) return r.stdout.trim();
  return null;
}

function dryGuard(action) {
  if (FLAGS.dryRun) {
    log(`[dry-run] would ${action}`);
    return true;
  }
  return false;
}

function ensureDir(dir) {
  if (dryGuard(`mkdir -p ${dir}`)) return;
  mkdirSync(dir, { recursive: true });
}

function copyDir(src, dest) {
  if (dryGuard(`cp -r ${src} ${dest}`)) return;
  mkdirSync(dirname(dest), { recursive: true });
  cpSync(src, dest, { recursive: true });
}

function copyFile(src, dest) {
  if (dryGuard(`cp ${src} ${dest}`)) return;
  mkdirSync(dirname(dest), { recursive: true });
  cpSync(src, dest);
}

function removePath(p) {
  if (!existsSync(p)) return false;
  if (dryGuard(`rm -rf ${p}`)) return true;
  rmSync(p, { recursive: true, force: true });
  return true;
}

function writeFile(path, contents) {
  if (dryGuard(`write ${path}`)) return;
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, contents);
}

function detectCLIs() {
  return {
    claude: which("claude"),
    gemini: which("gemini"),
    codex: which("codex"),
  };
}

function installClaudeSkill() {
  const src = join(PACKAGE_ROOT, "skills", "claude", SKILL_NAME);
  const dest = join(CLAUDE_SKILLS_DIR, SKILL_NAME);
  if (!existsSync(src)) {
    warn(`claude skill source missing at ${src}; skipping`);
    return { status: "skip", path: dest };
  }
  if (existsSync(dest) && !FLAGS.force) {
    warn(`${dest} already exists; pass --force to overwrite`);
    return { status: "exists", path: dest };
  }
  if (existsSync(dest)) removePath(dest);
  copyDir(src, dest);
  return { status: "installed", path: dest };
}

function installGeminiSkill(geminiBinary) {
  const src = join(PACKAGE_ROOT, "skills", "gemini", SKILL_NAME);
  const dest = join(GEMINI_SKILLS_DIR, SKILL_NAME);
  if (!existsSync(src)) {
    warn(`gemini skill source missing at ${src}; skipping`);
    return { status: "skip", path: dest };
  }
  if (geminiBinary && !FLAGS.dryRun && process.env.GROKRXIV_SKILLS_USE_GEMINI_CLI === "1") {
    const r = spawnSync(
      geminiBinary,
      ["skills", "install", src, "--scope", "user"],
      { encoding: "utf8", timeout: 15000, stdio: ["ignore", "pipe", "pipe"] },
    );
    if (r.status === 0) {
      return { status: "installed-via-cli", path: dest };
    }
    warn(
      `gemini skills install failed (status=${r.status}, signal=${r.signal}); falling back to direct copy`,
    );
  }
  if (existsSync(dest) && !FLAGS.force) {
    warn(`${dest} already exists; pass --force to overwrite`);
    return { status: "exists", path: dest };
  }
  if (existsSync(dest)) removePath(dest);
  copyDir(src, dest);
  return { status: "installed", path: dest };
}

function installCodexBlock() {
  const src = join(PACKAGE_ROOT, "skills", "codex", "AGENTS.md");
  if (!existsSync(src)) {
    warn(`codex AGENTS.md source missing at ${src}; skipping`);
    return { status: "skip", path: CODEX_AGENTS_FILE };
  }
  const block = readFileSync(src, "utf8").trim();
  let existing = "";
  if (existsSync(CODEX_AGENTS_FILE)) {
    existing = readFileSync(CODEX_AGENTS_FILE, "utf8");
  }
  const stripped = existing.replace(CODEX_BLOCK_REGEX, "\n").replace(/\n{3,}/g, "\n\n");
  const trimmed = stripped.replace(/\s+$/g, "");
  const sep = trimmed.length > 0 ? "\n\n" : "";
  const next = `${trimmed}${sep}${block}\n`;
  writeFile(CODEX_AGENTS_FILE, next);
  return { status: "installed", path: CODEX_AGENTS_FILE };
}

function copyBundledSchemas() {
  const src = join(PACKAGE_ROOT, "schemas");
  if (!existsSync(src)) {
    warn(`bundled schemas missing at ${src}; skipping`);
    return { status: "skip", path: SHARED_SCHEMAS_DIR };
  }
  ensureDir(SHARED_SCHEMAS_DIR);
  for (const f of readdirSync(src)) {
    if (!f.endsWith(".schema.json")) continue;
    copyFile(join(src, f), join(SHARED_SCHEMAS_DIR, f));
  }
  return { status: "installed", path: SHARED_SCHEMAS_DIR };
}

function cmdInstall() {
  const clis = detectCLIs();
  log(`@grokrxiv/skills v${PACKAGE_VERSION} — installing ${SKILL_NAME}`);
  log(
    `detected CLIs: claude=${clis.claude ? "yes" : "no"} gemini=${
      clis.gemini ? "yes" : "no"
    } codex=${clis.codex ? "yes" : "no"}`,
  );

  const results = {};
  results.claude = clis.claude
    ? installClaudeSkill()
    : { status: "cli-missing", path: join(CLAUDE_SKILLS_DIR, SKILL_NAME) };
  results.gemini = clis.gemini
    ? installGeminiSkill(clis.gemini)
    : { status: "cli-missing", path: join(GEMINI_SKILLS_DIR, SKILL_NAME) };
  results.codex = clis.codex
    ? installCodexBlock()
    : { status: "cli-missing", path: CODEX_AGENTS_FILE };
  results.schemas = copyBundledSchemas();

  printResults("install", results);
  const anyFailures = Object.values(results).some((r) =>
    ["error"].includes(r.status),
  );
  const anySkips = Object.values(results).some((r) =>
    ["cli-missing", "skip", "exists"].includes(r.status),
  );
  if (anyFailures) return 2;
  if (anySkips) return 1;
  return 0;
}

function cmdStatus() {
  const clis = detectCLIs();
  const results = {
    claude: {
      cli: !!clis.claude,
      installed: existsSync(join(CLAUDE_SKILLS_DIR, SKILL_NAME)),
      path: join(CLAUDE_SKILLS_DIR, SKILL_NAME),
    },
    gemini: {
      cli: !!clis.gemini,
      installed: existsSync(join(GEMINI_SKILLS_DIR, SKILL_NAME)),
      path: join(GEMINI_SKILLS_DIR, SKILL_NAME),
    },
    codex: {
      cli: !!clis.codex,
      installed: codexBlockPresent(),
      path: CODEX_AGENTS_FILE,
    },
    schemas: {
      cli: true,
      installed:
        existsSync(SHARED_SCHEMAS_DIR) &&
        readdirSync(SHARED_SCHEMAS_DIR).some((f) => f.endsWith(".schema.json")),
      path: SHARED_SCHEMAS_DIR,
    },
  };
  log(`@grokrxiv/skills v${PACKAGE_VERSION} — status`);
  for (const [name, r] of Object.entries(results)) {
    const cliMark = r.cli ? "yes" : "no";
    const installedMark = r.installed ? "yes" : "no";
    log(`  ${name.padEnd(10)} cli=${cliMark.padEnd(4)} installed=${installedMark.padEnd(4)} path=${r.path}`);
  }
  return 0;
}

function codexBlockPresent() {
  if (!existsSync(CODEX_AGENTS_FILE)) return false;
  const contents = readFileSync(CODEX_AGENTS_FILE, "utf8");
  return /<!--\s*BEGIN grokrxiv-skills v[^\s>]+\s*-->/.test(contents);
}

function cmdUninstall() {
  const results = {};
  results.claude = uninstallDir(join(CLAUDE_SKILLS_DIR, SKILL_NAME));
  results.gemini = uninstallDir(join(GEMINI_SKILLS_DIR, SKILL_NAME));
  results.codex = uninstallCodexBlock();
  results.schemas = uninstallDir(SHARED_SCHEMAS_DIR);
  printResults("uninstall", results);
  return 0;
}

function uninstallDir(path) {
  if (!existsSync(path)) return { status: "absent", path };
  removePath(path);
  return { status: "removed", path };
}

function uninstallCodexBlock() {
  if (!existsSync(CODEX_AGENTS_FILE)) return { status: "absent", path: CODEX_AGENTS_FILE };
  const existing = readFileSync(CODEX_AGENTS_FILE, "utf8");
  if (!/<!--\s*BEGIN grokrxiv-skills v[^\s>]+\s*-->/.test(existing)) {
    return { status: "absent", path: CODEX_AGENTS_FILE };
  }
  const stripped = existing
    .replace(CODEX_BLOCK_REGEX, "\n")
    .replace(/\n{3,}/g, "\n\n")
    .replace(/\s+$/g, "");
  const next = stripped.length === 0 ? "" : `${stripped}\n`;
  writeFile(CODEX_AGENTS_FILE, next);
  return { status: "removed", path: CODEX_AGENTS_FILE };
}

function cmdSyncSchemas() {
  const repoRoot = resolve(PACKAGE_ROOT, "..");
  const src = join(repoRoot, "schemas");
  if (!existsSync(src)) {
    err(`repo schemas dir not found at ${src}`);
    return 2;
  }
  const dest = join(PACKAGE_ROOT, "schemas");
  ensureDir(dest);
  const wanted = [
    "summary_review.schema.json",
    "technical_review.schema.json",
    "novelty_review.schema.json",
    "reproducibility_review.schema.json",
    "citation_review.schema.json",
    "meta_review.schema.json",
  ];
  let copied = 0;
  for (const f of wanted) {
    const s = join(src, f);
    if (!existsSync(s)) {
      warn(`source schema missing: ${s}`);
      continue;
    }
    copyFile(s, join(dest, f));
    copied++;
  }
  log(`sync-schemas: copied ${copied}/${wanted.length} schemas → ${dest}`);
  return copied === wanted.length ? 0 : 1;
}

function printResults(kind, results) {
  log(`${kind} results:`);
  for (const [name, r] of Object.entries(results)) {
    log(`  ${name.padEnd(10)} ${r.status.padEnd(20)} ${r.path || ""}`);
  }
}

function printHelp() {
  log(`@grokrxiv/skills v${PACKAGE_VERSION}

Usage:
  grokrxiv-skills <command> [flags]

Commands:
  install         Install grokrxiv-review skill into detected CLIs.
  uninstall       Remove the skill from all locations.
  status          Report install state. Does not write anything.
  sync-schemas    Re-copy <repo>/schemas/*.schema.json into the package.
  --help, -h      Show this help.

Flags:
  --force, -f     Overwrite existing skill directories.
  --dry-run, -n   Print actions without touching the filesystem.

Install locations:
  claude  -> ~/.claude/skills/${SKILL_NAME}/
  gemini  -> ~/.gemini/skills/${SKILL_NAME}/   (via 'gemini skills install' when available)
  codex   -> ~/.codex/AGENTS.md                (sentinel block, additive)
  schemas -> ~/.grokrxiv/skills/schemas/       (canonical JSON schemas)
`);
}

function main() {
  if (FLAGS.help || FLAGS.positional.length === 0) {
    printHelp();
    return 0;
  }
  const cmd = FLAGS.positional[0];
  switch (cmd) {
    case "install":
      return cmdInstall();
    case "uninstall":
      return cmdUninstall();
    case "status":
      return cmdStatus();
    case "sync-schemas":
      return cmdSyncSchemas();
    case "help":
    case "--help":
      printHelp();
      return 0;
    default:
      err(`unknown command: ${cmd}`);
      printHelp();
      return 2;
  }
}

const code = main();
process.exit(code ?? 0);
