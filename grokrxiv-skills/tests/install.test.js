// Tests for @grokrxiv/skills installer. Pure Node 18+ stdlib.
// Each test sets HOME to a fresh tempdir and invokes bin/install.js as a
// subprocess so we exercise the real CLI surface.

import { spawnSync } from "node:child_process";
import {
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import assert from "node:assert/strict";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const PKG_ROOT = resolve(dirname(__filename), "..");
const BIN = join(PKG_ROOT, "bin", "install.js");

function freshHome() {
  return mkdtempSync(join(tmpdir(), "grokrxiv-skills-test-"));
}

function runInstaller(args, home) {
  return spawnSync("node", [BIN, ...args], {
    env: { ...process.env, HOME: home },
    encoding: "utf8",
  });
}

function cleanup(home) {
  try {
    rmSync(home, { recursive: true, force: true });
  } catch {
    /* best-effort */
  }
}

test("install creates claude skill in tempdir HOME", () => {
  const home = freshHome();
  try {
    const r = runInstaller(["install", "--force"], home);
    assert.notEqual(r.status, 2, `installer failed: ${r.stderr}`);
    const skillPath = join(home, ".claude", "skills", "grokrxiv-review", "SKILL.md");
    assert.ok(
      existsSync(skillPath),
      `expected claude SKILL.md at ${skillPath}; stdout: ${r.stdout}`,
    );
    const contents = readFileSync(skillPath, "utf8");
    assert.match(contents, /name: grokrxiv-review/);
    assert.match(contents, /Output rules — STRICT/);
  } finally {
    cleanup(home);
  }
});

test("codex block is replaced, not duplicated, on reinstall", () => {
  const home = freshHome();
  try {
    const codexDir = join(home, ".codex");
    mkdirSync(codexDir, { recursive: true });
    const oldBlock = [
      "# Existing user content",
      "Some user notes that should be preserved.",
      "",
      "<!-- BEGIN grokrxiv-skills v0.0.1 -->",
      "# old version of the block",
      "stale content",
      "<!-- END grokrxiv-skills v0.0.1 -->",
      "",
      "More user notes after.",
    ].join("\n");
    writeFileSync(join(codexDir, "AGENTS.md"), oldBlock);

    const r1 = runInstaller(["install", "--force"], home);
    assert.notEqual(r1.status, 2, `first install failed: ${r1.stderr}`);

    const r2 = runInstaller(["install", "--force"], home);
    assert.notEqual(r2.status, 2, `second install failed: ${r2.stderr}`);

    const finalContents = readFileSync(join(codexDir, "AGENTS.md"), "utf8");
    const beginMatches = finalContents.match(/<!-- BEGIN grokrxiv-skills v/g) || [];
    const endMatches = finalContents.match(/<!-- END grokrxiv-skills v/g) || [];
    assert.equal(
      beginMatches.length,
      1,
      `expected exactly one BEGIN sentinel; got ${beginMatches.length}\n--- file ---\n${finalContents}`,
    );
    assert.equal(endMatches.length, 1, `expected exactly one END sentinel`);
    assert.ok(
      finalContents.includes("# Existing user content"),
      "pre-existing user content was lost",
    );
    assert.ok(
      finalContents.includes("More user notes after"),
      "trailing user content was lost",
    );
    assert.ok(
      !finalContents.includes("stale content"),
      "old block content was not stripped",
    );
    assert.ok(
      finalContents.includes("strict JSON output"),
      "new block content missing",
    );
  } finally {
    cleanup(home);
  }
});

test("status reports no install when clean", () => {
  const home = freshHome();
  try {
    const r = runInstaller(["status"], home);
    assert.equal(r.status, 0, `status should exit 0 even when nothing is installed; stderr: ${r.stderr}`);
    assert.match(r.stdout, /claude\s+cli=\S+\s+installed=no/);
    assert.match(r.stdout, /gemini\s+cli=\S+\s+installed=no/);
    assert.match(r.stdout, /codex\s+cli=\S+\s+installed=no/);
    assert.match(r.stdout, /schemas\s+cli=\S+\s+installed=no/);
  } finally {
    cleanup(home);
  }
});

test("uninstall is idempotent", () => {
  const home = freshHome();
  try {
    runInstaller(["install", "--force"], home);
    const r1 = runInstaller(["uninstall"], home);
    assert.equal(r1.status, 0, `first uninstall should succeed: ${r1.stderr}`);
    const r2 = runInstaller(["uninstall"], home);
    assert.equal(r2.status, 0, `second uninstall should succeed: ${r2.stderr}`);
    assert.ok(
      !existsSync(join(home, ".claude", "skills", "grokrxiv-review")),
      "claude skill dir should be gone",
    );
    assert.ok(
      !existsSync(join(home, ".grokrxiv", "skills", "schemas")),
      "shared schemas dir should be gone",
    );
    const codexPath = join(home, ".codex", "AGENTS.md");
    if (existsSync(codexPath)) {
      const contents = readFileSync(codexPath, "utf8");
      assert.ok(
        !contents.includes("<!-- BEGIN grokrxiv-skills"),
        "codex block should be stripped",
      );
    }
  } finally {
    cleanup(home);
  }
});

test("sync-schemas copies from repo schemas/", () => {
  const home = freshHome();
  try {
    const r = runInstaller(["sync-schemas"], home);
    assert.equal(r.status, 0, `sync-schemas failed: ${r.stderr}`);
    const pkgSchemas = join(PKG_ROOT, "schemas");
    const wanted = [
      "summary_review.schema.json",
      "technical_review.schema.json",
      "novelty_review.schema.json",
      "reproducibility_review.schema.json",
      "citation_review.schema.json",
      "meta_review.schema.json",
    ];
    for (const f of wanted) {
      const p = join(pkgSchemas, f);
      assert.ok(existsSync(p), `expected schema present after sync: ${p}`);
      const j = JSON.parse(readFileSync(p, "utf8"));
      assert.ok(j.title, `schema ${f} must have a title`);
    }
  } finally {
    cleanup(home);
  }
});
