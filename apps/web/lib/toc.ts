export interface TocItem {
  id: string;
  text: string;
  level: 2 | 3;
}

const HEADING_RE = /^(#{2,3})\s+(.+?)\s*#*\s*$/;
const FENCE_RE = /^```/;

/**
 * Build a list of TOC entries from a markdown source string. Scans top-level
 * ## / ### ATX headings and slugifies them using the same rule the heading
 * id pass in `markdown-body.tsx` uses, so the anchors on the rendered page
 * line up with these ids.
 */
export function buildTocFromMarkdown(md: string): TocItem[] {
  const out: TocItem[] = [];
  const seen = new Map<string, number>();

  const lines = md.split("\n");
  let inCodeFence = false;
  for (const line of lines) {
    if (FENCE_RE.test(line.trim())) {
      inCodeFence = !inCodeFence;
      continue;
    }
    if (inCodeFence) continue;
    const m = line.match(HEADING_RE);
    if (!m) continue;
    const level = m[1].length as 2 | 3;
    const text = m[2].replace(/[*_`]/g, "").trim();
    const baseId = slugify(text);
    if (!baseId) continue;
    let id = baseId;
    const n = seen.get(baseId) ?? 0;
    if (n > 0) id = `${baseId}-${n}`;
    seen.set(baseId, n + 1);
    out.push({ id, text, level });
  }
  return out;
}

function slugify(s: string): string {
  return s
    .toLowerCase()
    .replace(/[^\w\s-]/g, "")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-|-$/g, "");
}
