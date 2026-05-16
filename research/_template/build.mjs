#!/usr/bin/env node
/**
 * grokrxiv research build — markdown → self-contained dark-themed HTML.
 *
 * Usage:
 *   node build.mjs                       # builds every research/*.md
 *   node build.mjs <path/to/file.md>     # builds one file
 *
 * Output: research/{basename}.html
 *
 * Idempotent: with GROKRXIV_BUILD_DATE set (or the source mtime), repeated runs
 * produce byte-identical HTML.
 */

import { readFile, writeFile, readdir, stat } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import process from 'node:process';

import { unified } from 'unified';
import remarkParse from 'remark-parse';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';
import remarkRehype from 'remark-rehype';
import rehypeSlug from 'rehype-slug';
import rehypeAutolinkHeadings from 'rehype-autolink-headings';
import rehypeKatex from 'rehype-katex';
import rehypePrettyCode from 'rehype-pretty-code';
import rehypeStringify from 'rehype-stringify';
import matter from 'gray-matter';

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const REPO_ROOT = path.resolve(__dirname, '..', '..');
const RESEARCH_DIR = path.resolve(__dirname, '..');

// ---------- helpers ----------

async function readUtf8(p) {
  return await readFile(p, 'utf8');
}

function escapeHtml(s) {
  return String(s)
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}

function extractFirstH1(markdown) {
  const lines = markdown.split('\n');
  for (const line of lines) {
    const m = line.match(/^#\s+(.+?)\s*$/);
    if (m) return m[1].trim();
  }
  return null;
}

function resolveInputPath(input) {
  if (path.isAbsolute(input)) return path.resolve(input);
  // Try relative to CWD first, then repo root, then research dir
  const candidates = [
    path.resolve(process.cwd(), input),
    path.resolve(REPO_ROOT, input),
    path.resolve(RESEARCH_DIR, input),
  ];
  for (const c of candidates) {
    try {
      // synchronous-ish existence check via stat in async caller; we'll just return the first
      return c;
    } catch { /* skip */ }
  }
  return path.resolve(process.cwd(), input);
}

async function fileExists(p) {
  try {
    await stat(p);
    return true;
  } catch {
    return false;
  }
}

async function resolveAndCheck(input) {
  if (path.isAbsolute(input)) {
    if (await fileExists(input)) return input;
  }
  for (const base of [process.cwd(), REPO_ROOT, RESEARCH_DIR]) {
    const c = path.resolve(base, input);
    if (await fileExists(c)) return c;
  }
  throw new Error(`Input file not found: ${input}`);
}

// ---------- pipeline ----------

function buildProcessor() {
  return unified()
    .use(remarkParse)
    .use(remarkGfm)
    .use(remarkMath)
    .use(remarkRehype, { allowDangerousHtml: true })
    .use(rehypeSlug)
    .use(rehypeAutolinkHeadings, {
      behavior: 'prepend',
      properties: { className: ['anchor'], ariaLabel: 'anchor' },
      content: { type: 'text', value: '#' },
    })
    .use(rehypeKatex)
    .use(rehypePrettyCode, {
      theme: 'github-dark',
      keepBackground: false,
    })
    .use(rehypeStringify, { allowDangerousHtml: true });
}

async function loadInlineCss() {
  const customCss = await readUtf8(path.join(__dirname, 'style.css'));
  // KaTeX CSS lives in node_modules; we have to rewrite font URLs to absolute
  // because the inlined CSS will be loaded from a file:// HTML doc that has no
  // base path matching node_modules. We can either inline-fontless (acceptable
  // since math is rare) or strip @font-face. Strip @font-face to keep output
  // self-contained without external font deps.
  let katexCss = '';
  try {
    katexCss = await readUtf8(
      path.join(__dirname, 'node_modules', 'katex', 'dist', 'katex.min.css'),
    );
    // Strip @font-face blocks — they reference fonts/ which won't resolve from
    // a file:// HTML opened anywhere on disk. KaTeX falls back to system fonts
    // and inline glyphs still render acceptably for our doc set.
    katexCss = katexCss.replace(/@font-face\s*\{[^}]*\}/g, '');
  } catch {
    // KaTeX optional — if not installed, math just won't be styled.
  }
  return { customCss, katexCss };
}

function wrapDocument({ title, bodyHtml, customCss, katexCss, filename, buildDate }) {
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>${escapeHtml(title)}</title>
<style>
${katexCss}
${customCss}
</style>
</head>
<body>
<main class="prose">
${bodyHtml}
</main>
<footer class="doc-footer">
<p>${escapeHtml(filename)} &middot; generated ${escapeHtml(buildDate)} &middot; grokrxiv</p>
</footer>
</body>
</html>
`;
}

async function buildOne(inputPath, { processor, customCss, katexCss }) {
  const abs = await resolveAndCheck(inputPath);
  const raw = await readUtf8(abs);
  const parsed = matter(raw); // strips frontmatter if any
  const md = parsed.content;

  const file = await processor.process(md);
  const bodyHtml = String(file);

  const basename = path.basename(abs, path.extname(abs));
  const filename = `${basename}.md`;

  const titleFromDoc = extractFirstH1(md);
  const title = titleFromDoc || basename;

  // Idempotent build date: env override > source mtime ISO
  let buildDate;
  if (process.env.GROKRXIV_BUILD_DATE) {
    buildDate = process.env.GROKRXIV_BUILD_DATE;
  } else {
    const s = await stat(abs);
    buildDate = s.mtime.toISOString();
  }

  const html = wrapDocument({ title, bodyHtml, customCss, katexCss, filename, buildDate });

  const outPath = path.join(RESEARCH_DIR, `${basename}.html`);
  await writeFile(outPath, html, 'utf8');

  const sizeKb = (Buffer.byteLength(html, 'utf8') / 1024).toFixed(1);
  const rel = path.relative(REPO_ROOT, outPath);
  console.log(`Generated: ${rel} (${sizeKb} KB)`);
}

async function listResearchMarkdown() {
  const entries = await readdir(RESEARCH_DIR, { withFileTypes: true });
  return entries
    .filter((e) => e.isFile() && e.name.endsWith('.md'))
    .map((e) => path.join(RESEARCH_DIR, e.name))
    .sort();
}

// ---------- main ----------

async function main() {
  const args = process.argv.slice(2);
  const processor = buildProcessor();
  const { customCss, katexCss } = await loadInlineCss();

  const targets =
    args.length === 0 ? await listResearchMarkdown() : args;

  if (targets.length === 0) {
    console.log('No markdown files found in research/.');
    return;
  }

  for (const t of targets) {
    try {
      await buildOne(t, { processor, customCss, katexCss });
    } catch (err) {
      console.error(`Failed: ${t}\n  ${err.message}`);
      process.exitCode = 1;
    }
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
