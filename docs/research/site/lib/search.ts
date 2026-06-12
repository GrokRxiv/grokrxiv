import { promises as fs } from 'node:fs';
import path from 'node:path';
import { RESEARCH_DIR, listDocs } from './research-docs';

export type SearchEntry = {
  slug: string;
  title: string;
  body: string;
};

/**
 * Decode the small handful of HTML entities we expect to see. Mirrors
 * research-docs.ts but kept private so the modules don't form a cycle on a
 * single helper.
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

function stripAll(html: string): string {
  return decodeEntities(
    html
      .replace(/<style[\s\S]*?<\/style>/gi, '')
      .replace(/<script[\s\S]*?<\/script>/gi, '')
      .replace(/<[^>]+>/g, ' ')
  )
    .replace(/\s+/g, ' ')
    .trim();
}

/**
 * Build the full search index by walking the HTML files and stripping tags.
 * Used by the search box (client component) — passed in as a prop from the
 * server side so no API route is needed.
 */
export async function buildSearchIndex(): Promise<SearchEntry[]> {
  const docs = await listDocs();
  const entries = await Promise.all(
    docs.map(async (doc): Promise<SearchEntry | null> => {
      try {
        const html = await fs.readFile(path.join(RESEARCH_DIR, `${doc.slug}.html`), 'utf8');
        return {
          slug: doc.slug,
          title: doc.title,
          body: stripAll(html),
        };
      } catch {
        return null;
      }
    })
  );
  return entries.filter((e): e is SearchEntry => e !== null);
}
