import { promises as fs } from 'node:fs';
import path from 'node:path';

// Resolution: research/site/ is the cwd for Next.js. `..` from there is
// research/, which holds the generated *.html artifacts from Track B.
// The HTML files we care about live at research/*.html — siblings of this
// site/ directory. Directories like research/_template/ and research/site/
// itself are filtered out by the .html extension check (they are dirs,
// not files).
export const RESEARCH_DIR = path.resolve(process.cwd(), '..');

export type ResearchDoc = {
  slug: string;
  title: string;
  summary: string;
  wordCount: number;
  mtimeISO: string;
};

/**
 * Decode the small handful of HTML entities we expect to see in titles and
 * leading paragraphs from KaTeX / markdown-it output. Not a full decoder;
 * just enough to make summaries readable.
 */
function decodeEntities(s: string): string {
  return s
    .replace(/&amp;/g, '&')
    .replace(/&lt;/g, '<')
    .replace(/&gt;/g, '>')
    .replace(/&quot;/g, '"')
    .replace(/&#39;/g, "'")
    .replace(/&apos;/g, "'")
    .replace(/&nbsp;/g, ' ')
    .replace(/&mdash;/g, '—')
    .replace(/&ndash;/g, '–')
    .replace(/&hellip;/g, '…');
}

/** Strip every HTML tag and collapse whitespace. */
function stripTags(html: string): string {
  return decodeEntities(html.replace(/<[^>]*>/g, '')).replace(/\s+/g, ' ').trim();
}

/** Extract body content (everything between `<body...>` and `</body>`). */
function extractBody(html: string): string {
  const open = html.search(/<body[^>]*>/i);
  if (open === -1) return html;
  const afterOpen = html.indexOf('>', open) + 1;
  const close = html.toLowerCase().lastIndexOf('</body>');
  return close === -1 ? html.slice(afterOpen) : html.slice(afterOpen, close);
}

/** Remove blocks that often shadow the "first paragraph" lookup. */
function dropNoise(body: string): string {
  return body
    .replace(/<style[\s\S]*?<\/style>/gi, '')
    .replace(/<script[\s\S]*?<\/script>/gi, '')
    .replace(/<pre[\s\S]*?<\/pre>/gi, '');
}

function parseTitle(html: string, fallback: string): string {
  const m = html.match(/<h1\b[^>]*>([\s\S]*?)<\/h1>/i);
  if (m) {
    const cleaned = stripTags(m[1]);
    if (cleaned) return cleaned;
  }
  // Fall back to <title>...</title>.
  const t = html.match(/<title>([\s\S]*?)<\/title>/i);
  if (t) {
    const cleaned = stripTags(t[1]);
    if (cleaned) return cleaned;
  }
  return fallback;
}

function parseSummary(body: string, maxChars = 200): string {
  const m = body.match(/<p\b[^>]*>([\s\S]*?)<\/p>/i);
  if (!m) return '';
  const text = stripTags(m[1]);
  if (text.length <= maxChars) return text;
  return text.slice(0, maxChars - 1).trimEnd() + '…';
}

function wordCountFromBody(body: string): number {
  // Rough: tag-stripped character length / 5. Cheap and predictable.
  const text = stripTags(body);
  return Math.max(0, Math.round(text.length / 5));
}

/** List all HTML docs siblings of research/site/, newest first. */
export async function listDocs(): Promise<ResearchDoc[]> {
  let entries: import('node:fs').Dirent[];
  try {
    entries = await fs.readdir(RESEARCH_DIR, { withFileTypes: true });
  } catch {
    return [];
  }

  const htmlNames = entries
    .filter((e) => e.isFile() && e.name.endsWith('.html'))
    .map((e) => e.name);

  const docs = await Promise.all(
    htmlNames.map(async (name): Promise<ResearchDoc | null> => {
      const slug = name.replace(/\.html$/, '');
      const full = path.join(RESEARCH_DIR, name);
      try {
        const [stat, html] = await Promise.all([fs.stat(full), fs.readFile(full, 'utf8')]);
        const body = dropNoise(extractBody(html));
        return {
          slug,
          title: parseTitle(html, slug),
          summary: parseSummary(body),
          wordCount: wordCountFromBody(body),
          mtimeISO: stat.mtime.toISOString(),
        };
      } catch {
        return null;
      }
    })
  );

  return docs
    .filter((d): d is ResearchDoc => d !== null)
    .sort((a, b) => (a.mtimeISO < b.mtimeISO ? 1 : -1));
}

/** Read full HTML for a slug, or null when missing. */
export async function readDocHtml(slug: string): Promise<string | null> {
  // Guard against path traversal: slug must be a plain filename component.
  if (!/^[A-Za-z0-9._-]+$/.test(slug)) return null;
  const full = path.join(RESEARCH_DIR, `${slug}.html`);
  try {
    return await fs.readFile(full, 'utf8');
  } catch {
    return null;
  }
}
