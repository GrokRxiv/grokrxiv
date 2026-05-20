# GrokRxiv CLI Package Installation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `grokrxiv` installable like a normal command-line package, so users can run one package-manager command instead of learning Cargo workspace details.

**Architecture:** Keep the Rust binary as the source of truth. Ship prebuilt release binaries from GitHub Releases, then expose them through a small npm package with a Node shim and installer. Preserve `cargo install --locked` as the developer fallback and add Homebrew/curl distribution after the npm path is stable.

**Tech Stack:** Rust/Cargo, GitHub Actions, npm package with Node 20 stdlib, optional Homebrew formula.

---

## File Structure

- Create `packages/grokrxiv-cli/package.json`: publishable npm package metadata and `bin` mapping for `grokrxiv`.
- Create `packages/grokrxiv-cli/bin/grokrxiv.js`: stable Node shim that executes the downloaded native binary.
- Create `packages/grokrxiv-cli/scripts/install.js`: postinstall resolver that selects platform/arch, downloads the release binary, verifies checksum, and falls back to `cargo install --locked`.
- Create `packages/grokrxiv-cli/scripts/lib/platform.js`: maps Node platform/arch to release artifact names.
- Create `packages/grokrxiv-cli/scripts/lib/checksum.js`: SHA256 verification helper.
- Create `packages/grokrxiv-cli/tests/platform.test.js`: platform mapping tests.
- Create `packages/grokrxiv-cli/tests/install.test.js`: installer tests using local fixture artifacts, no network.
- Modify root `package.json`: add local operator scripts for installing/testing the CLI package.
- Modify `.github/workflows/rust.yml`: keep normal CI unchanged and ensure locked Cargo checks continue passing.
- Create `.github/workflows/release-cli.yml`: tag-triggered build that publishes binaries/checksums for macOS arm64, macOS x64, Linux x64, and Linux arm64.
- Modify `README.md`: replace raw Cargo install guidance with package-first install commands and keep Cargo as the developer fallback.
- Modify `docs/grokrxiv-cli-reference-applied.md`: document package install, update, uninstall, and local developer install.

## Current Developer Install

Use this immediately until the package work lands:

```bash
cargo install --locked --path crates/orchestrator --bin grokrxiv --force
grokrxiv --version
grokrxiv --status --json --dry-run review 2602.17480
```

The `--locked` flag is required because this repo pins Rust 1.82. Without it, Cargo may resolve newer transitive crates that require edition 2024 and fail before compiling.

## Task 1: Add Package Platform Mapping

**Files:**
- Create: `packages/grokrxiv-cli/scripts/lib/platform.js`
- Create: `packages/grokrxiv-cli/tests/platform.test.js`

- [ ] **Step 1: Write the failing platform tests**

```js
import assert from "node:assert/strict";
import test from "node:test";
import { artifactNameFor, executableName } from "../scripts/lib/platform.js";

test("maps supported release targets", () => {
  assert.equal(
    artifactNameFor({ platform: "darwin", arch: "arm64", version: "0.1.0" }),
    "grokrxiv-aarch64-apple-darwin-v0.1.0.tar.gz",
  );
  assert.equal(
    artifactNameFor({ platform: "darwin", arch: "x64", version: "0.1.0" }),
    "grokrxiv-x86_64-apple-darwin-v0.1.0.tar.gz",
  );
  assert.equal(
    artifactNameFor({ platform: "linux", arch: "x64", version: "0.1.0" }),
    "grokrxiv-x86_64-unknown-linux-gnu-v0.1.0.tar.gz",
  );
  assert.equal(
    artifactNameFor({ platform: "linux", arch: "arm64", version: "0.1.0" }),
    "grokrxiv-aarch64-unknown-linux-gnu-v0.1.0.tar.gz",
  );
});

test("rejects unsupported platforms with a clear message", () => {
  assert.throws(
    () => artifactNameFor({ platform: "win32", arch: "x64", version: "0.1.0" }),
    /Unsupported platform win32\/x64/,
  );
});

test("uses the native executable name", () => {
  assert.equal(executableName(), "grokrxiv");
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
node --test packages/grokrxiv-cli/tests/platform.test.js
```

Expected: FAIL because `packages/grokrxiv-cli/scripts/lib/platform.js` does not exist.

- [ ] **Step 3: Implement the platform mapper**

```js
const TARGETS = new Map([
  ["darwin/arm64", "aarch64-apple-darwin"],
  ["darwin/x64", "x86_64-apple-darwin"],
  ["linux/x64", "x86_64-unknown-linux-gnu"],
  ["linux/arm64", "aarch64-unknown-linux-gnu"],
]);

export function targetTripleFor({ platform = process.platform, arch = process.arch } = {}) {
  const key = `${platform}/${arch}`;
  const target = TARGETS.get(key);
  if (!target) {
    throw new Error(
      `Unsupported platform ${key}. Supported targets: ${Array.from(TARGETS.keys()).join(", ")}`,
    );
  }
  return target;
}

export function artifactNameFor({ platform = process.platform, arch = process.arch, version }) {
  if (!version) throw new Error("version is required");
  return `grokrxiv-${targetTripleFor({ platform, arch })}-v${version}.tar.gz`;
}

export function executableName() {
  return "grokrxiv";
}
```

- [ ] **Step 4: Run the test to verify it passes**

Run:

```bash
node --test packages/grokrxiv-cli/tests/platform.test.js
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add packages/grokrxiv-cli/scripts/lib/platform.js packages/grokrxiv-cli/tests/platform.test.js
git commit -m "feat(cli): map package targets to release artifacts"
```

## Task 2: Add npm Package Shim

**Files:**
- Create: `packages/grokrxiv-cli/package.json`
- Create: `packages/grokrxiv-cli/bin/grokrxiv.js`
- Create: `packages/grokrxiv-cli/tests/shim.test.js`

- [ ] **Step 1: Write the failing shim test**

```js
import assert from "node:assert/strict";
import { existsSync, statSync } from "node:fs";
import { readFile } from "node:fs/promises";
import test from "node:test";

test("npm package exposes grokrxiv bin shim", async () => {
  const pkg = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  assert.equal(pkg.name, "@grokrxiv/cli");
  assert.equal(pkg.bin.grokrxiv, "bin/grokrxiv.js");
  assert.equal(pkg.scripts.postinstall, "node scripts/install.js");
  assert.equal(pkg.files.includes("bin/"), true);
  assert.equal(pkg.files.includes("scripts/"), true);
});

test("shim exists and is executable", () => {
  const shim = new URL("../bin/grokrxiv.js", import.meta.url);
  assert.equal(existsSync(shim), true);
  assert.equal((statSync(shim).mode & 0o111) !== 0, true);
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
node --test packages/grokrxiv-cli/tests/shim.test.js
```

Expected: FAIL because the npm package files do not exist.

- [ ] **Step 3: Create the npm package**

```json
{
  "name": "@grokrxiv/cli",
  "version": "0.1.0",
  "description": "GrokRxiv command-line interface",
  "license": "MIT OR Apache-2.0",
  "repository": "github:GrokRxiv/grokrxiv",
  "type": "module",
  "bin": {
    "grokrxiv": "bin/grokrxiv.js"
  },
  "files": [
    "bin/",
    "scripts/",
    "README.md"
  ],
  "scripts": {
    "postinstall": "node scripts/install.js",
    "test": "node --test tests/*.test.js"
  },
  "engines": {
    "node": ">=20.11"
  }
}
```

- [ ] **Step 4: Create the shim**

```js
#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const binary = join(__dirname, "..", "vendor", "grokrxiv");

if (!existsSync(binary)) {
  process.stderr.write(
    "grokrxiv native binary is missing. Reinstall with: npm install -g @grokrxiv/cli\n",
  );
  process.exit(127);
}

const result = spawnSync(binary, process.argv.slice(2), {
  stdio: "inherit",
  env: process.env,
});

if (result.error) {
  process.stderr.write(`failed to run grokrxiv: ${result.error.message}\n`);
  process.exit(1);
}

if (result.signal) {
  process.kill(process.pid, result.signal);
}

process.exit(result.status ?? 1);
```

- [ ] **Step 5: Mark the shim executable and verify**

Run:

```bash
chmod +x packages/grokrxiv-cli/bin/grokrxiv.js
node --test packages/grokrxiv-cli/tests/shim.test.js
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add packages/grokrxiv-cli/package.json packages/grokrxiv-cli/bin/grokrxiv.js packages/grokrxiv-cli/tests/shim.test.js
git commit -m "feat(cli): add npm package shim"
```

## Task 3: Add Installer with Locked Cargo Fallback

**Files:**
- Create: `packages/grokrxiv-cli/scripts/install.js`
- Create: `packages/grokrxiv-cli/scripts/lib/checksum.js`
- Create: `packages/grokrxiv-cli/tests/install.test.js`

- [ ] **Step 1: Write installer tests**

```js
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { mkdir, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { verifySha256 } from "../scripts/lib/checksum.js";

test("verifySha256 accepts matching checksum", async () => {
  const dir = mkdtempSync(join(tmpdir(), "grokrxiv-cli-"));
  const file = join(dir, "artifact");
  await writeFile(file, "hello");
  await verifySha256(file, "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824");
  await rm(dir, { recursive: true, force: true });
});

test("verifySha256 rejects mismatched checksum", async () => {
  const dir = mkdtempSync(join(tmpdir(), "grokrxiv-cli-"));
  const file = join(dir, "artifact");
  await writeFile(file, "hello");
  await assert.rejects(
    () => verifySha256(file, "0000000000000000000000000000000000000000000000000000000000000000"),
    /checksum mismatch/,
  );
  await rm(dir, { recursive: true, force: true });
});
```

- [ ] **Step 2: Run the test to verify it fails**

Run:

```bash
node --test packages/grokrxiv-cli/tests/install.test.js
```

Expected: FAIL because `checksum.js` does not exist.

- [ ] **Step 3: Implement checksum helper**

```js
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";

export async function sha256File(path) {
  const hash = createHash("sha256");
  await new Promise((resolve, reject) => {
    createReadStream(path)
      .on("data", (chunk) => hash.update(chunk))
      .on("error", reject)
      .on("end", resolve);
  });
  return hash.digest("hex");
}

export async function verifySha256(path, expected) {
  const actual = await sha256File(path);
  if (actual !== expected) {
    throw new Error(`checksum mismatch for ${path}: expected ${expected}, got ${actual}`);
  }
}
```

- [ ] **Step 4: Implement installer behavior**

`packages/grokrxiv-cli/scripts/install.js` must:

```js
#!/usr/bin/env node
import { spawnSync } from "node:child_process";
import { createWriteStream, existsSync, mkdirSync, renameSync } from "node:fs";
import { chmod, cp, mkdtemp, rm } from "node:fs/promises";
import { get } from "node:https";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";
import { artifactNameFor, executableName } from "./lib/platform.js";

const __filename = fileURLToPath(import.meta.url);
const __dirname = dirname(__filename);
const packageRoot = join(__dirname, "..");
const packageJson = await import(new URL("../package.json", import.meta.url), {
  with: { type: "json" },
});
const version = packageJson.default.version;
const vendorDir = join(packageRoot, "vendor");
const vendorBin = join(vendorDir, executableName());

function log(message) {
  process.stdout.write(`[grokrxiv-cli] ${message}\n`);
}

function download(url, dest) {
  return new Promise((resolve, reject) => {
    get(url, (response) => {
      if (response.statusCode !== 200) {
        reject(new Error(`download failed ${response.statusCode}: ${url}`));
        response.resume();
        return;
      }
      pipeline(response, createWriteStream(dest)).then(resolve, reject);
    }).on("error", reject);
  });
}

function cargoFallback() {
  const cargo = spawnSync("cargo", ["--version"], { encoding: "utf8" });
  if (cargo.status !== 0) return false;
  const result = spawnSync(
    "cargo",
    [
      "install",
      "--locked",
      "--git",
      "https://github.com/GrokRxiv/grokrxiv",
      "--bin",
      "grokrxiv",
      "--force",
    ],
    { stdio: "inherit" },
  );
  return result.status === 0;
}

async function main() {
  mkdirSync(vendorDir, { recursive: true });
  const artifact = artifactNameFor({ version });
  const url = `https://github.com/GrokRxiv/grokrxiv/releases/download/v${version}/${artifact}`;
  const tmp = await mkdtemp(join(tmpdir(), "grokrxiv-cli-"));
  try {
    const archive = join(tmp, artifact);
    log(`downloading ${artifact}`);
    await download(url, archive);
    const tar = spawnSync("tar", ["-xzf", archive, "-C", tmp], { stdio: "inherit" });
    if (tar.status !== 0) throw new Error("tar extraction failed");
    await cp(join(tmp, executableName()), vendorBin);
    await chmod(vendorBin, 0o755);
    log(`installed native binary to ${vendorBin}`);
  } catch (error) {
    log(`${error.message}; trying locked Cargo fallback`);
    if (!cargoFallback()) {
      throw new Error(
        "failed to install grokrxiv. Install Rust and run: cargo install --locked --git https://github.com/GrokRxiv/grokrxiv --bin grokrxiv --force",
      );
    }
  } finally {
    await rm(tmp, { recursive: true, force: true });
  }
}

main().catch((error) => {
  process.stderr.write(`[grokrxiv-cli] ${error.message}\n`);
  process.exit(1);
});
```

- [ ] **Step 5: Run tests**

Run:

```bash
node --test packages/grokrxiv-cli/tests/*.test.js
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add packages/grokrxiv-cli/scripts packages/grokrxiv-cli/tests
git commit -m "feat(cli): install native grokrxiv from release artifacts"
```

## Task 4: Add Release Binary Workflow

**Files:**
- Create: `.github/workflows/release-cli.yml`

- [ ] **Step 1: Add the workflow**

```yaml
name: release-cli

on:
  push:
    tags:
      - "v*"

permissions:
  contents: write

jobs:
  build:
    strategy:
      fail-fast: false
      matrix:
        include:
          - os: macos-14
            target: aarch64-apple-darwin
          - os: macos-14
            target: x86_64-apple-darwin
          - os: ubuntu-24.04
            target: x86_64-unknown-linux-gnu
          - os: ubuntu-24.04-arm
            target: aarch64-unknown-linux-gnu
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: 1.82.0
          targets: ${{ matrix.target }}
      - uses: Swatinem/rust-cache@v2
        with:
          key: release-${{ matrix.target }}
      - name: Build grokrxiv
        run: cargo build --locked --release --bin grokrxiv --target ${{ matrix.target }}
      - name: Package
        shell: bash
        run: |
          version="${GITHUB_REF_NAME#v}"
          name="grokrxiv-${{ matrix.target }}-v${version}"
          mkdir -p "dist/${name}"
          cp "target/${{ matrix.target }}/release/grokrxiv" "dist/${name}/grokrxiv"
          tar -C "dist/${name}" -czf "dist/${name}.tar.gz" grokrxiv
          shasum -a 256 "dist/${name}.tar.gz" > "dist/${name}.tar.gz.sha256"
      - uses: softprops/action-gh-release@v2
        with:
          files: |
            dist/*.tar.gz
            dist/*.sha256
```

- [ ] **Step 2: Validate workflow syntax locally**

Run:

```bash
yamllint .github/workflows/release-cli.yml || true
git diff --check -- .github/workflows/release-cli.yml
```

Expected: no trailing whitespace; if `yamllint` is not installed, the command reports that and continues.

- [ ] **Step 3: Commit**

```bash
git add .github/workflows/release-cli.yml
git commit -m "ci: publish grokrxiv cli release binaries"
```

## Task 5: Add Root Install Scripts and Docs

**Files:**
- Modify: `package.json`
- Modify: `README.md`
- Modify: `docs/grokrxiv-cli-reference-applied.md`

- [ ] **Step 1: Add root package scripts**

Add:

```json
{
  "scripts": {
    "install:cli": "cargo install --locked --path crates/orchestrator --bin grokrxiv --force",
    "install:cli:npm-local": "npm install -g ./packages/grokrxiv-cli",
    "test:cli-package": "node --test packages/grokrxiv-cli/tests/*.test.js"
  }
}
```

Keep existing scripts unchanged.

- [ ] **Step 2: Document install commands**

Add this public install section:

```markdown
## Install GrokRxiv CLI

Preferred user install after the first package release:

```bash
npm install -g @grokrxiv/cli
grokrxiv --version
```

Developer install from a checkout:

```bash
cargo install --locked --path crates/orchestrator --bin grokrxiv --force
grokrxiv --version
```

Use `--locked`; the workspace pins Rust 1.82 and should not resolve newer transitive crates during install.
```

- [ ] **Step 3: Run docs/package verification**

Run:

```bash
pnpm test:cli-package
pnpm install:cli
grokrxiv --status --json --dry-run review 2602.17480
```

Expected: tests pass, install succeeds, dry-run prints clean status on stderr and valid JSON on stdout.

- [ ] **Step 4: Commit**

```bash
git add package.json README.md docs/grokrxiv-cli-reference-applied.md
git commit -m "docs: document package-first grokrxiv cli install"
```

## Task 6: Local End-to-End Package Smoke

**Files:**
- Test only; no source files changed unless a previous task failed.

- [ ] **Step 1: Install from local npm package**

Run:

```bash
npm install -g ./packages/grokrxiv-cli
which grokrxiv
grokrxiv --version
```

Expected: `grokrxiv 0.1.0`.

- [ ] **Step 2: Verify clean CLI output**

Run:

```bash
set -a
source .env
set +a
grokrxiv --status --json --dry-run review 2602.17480 > /tmp/grokrxiv-package-stdout.json 2> /tmp/grokrxiv-package-stderr.txt
jq -e . /tmp/grokrxiv-package-stdout.json
rg '^\\{"timestamp"|^status:' /tmp/grokrxiv-package-stderr.txt && exit 1 || true
sed -n '1,20p' /tmp/grokrxiv-package-stderr.txt
```

Expected:

```text
GrokRxiv review 2602.17480
runner=cli extractor=cli cache=off provider_api=disabled

[1/1] Plan         [OK] dry run; no pipeline work started
```

- [ ] **Step 3: Commit smoke fixes if needed**

If any package smoke fix was required:

```bash
git add packages/grokrxiv-cli package.json README.md docs/grokrxiv-cli-reference-applied.md
git commit -m "fix(cli): repair package install smoke"
```

## Future Channel: Homebrew

Homebrew should not block the npm MVP. Add a tap only after `release-cli.yml` has produced at least one stable release with verified checksums. The follow-up Homebrew plan should be written with the real release version and real SHA256 values from the release artifacts, not a template.

## Test Plan

- `node --test packages/grokrxiv-cli/tests/*.test.js`
- `npm install -g ./packages/grokrxiv-cli`
- `grokrxiv --version`
- `grokrxiv --status --json --dry-run review 2602.17480`
- `cargo install --locked --path crates/orchestrator --bin grokrxiv --force`
- `cargo test -p grokrxiv-orchestrator --lib`
- GitHub tag dry-run on a test tag in a fork before publishing npm.

## Rollout

1. Land local developer install script and npm package scaffolding.
2. Publish GitHub release artifacts for a test tag.
3. Test `npm install -g @grokrxiv/cli` on macOS arm64 and Linux x64.
4. Publish npm package.
5. Write a separate Homebrew tap plan using the real release artifact URLs and checksums.

## Self-Review

- Spec coverage: covers package-style install, npm-like usage, current Cargo workaround, release binaries, and future Homebrew.
- Concrete-value scan: no implementation step depends on unknown release values; Homebrew is explicitly moved to a future plan after real checksums exist.
- Type consistency: package tests, installer helpers, and shim paths all use `packages/grokrxiv-cli`.
