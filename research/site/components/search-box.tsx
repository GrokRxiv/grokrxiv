'use client';

import Link from 'next/link';
import { Fragment, useMemo, useState } from 'react';
import type { ReactNode } from 'react';
import Fuse from 'fuse.js';
import type { SearchEntry } from '@/lib/search';

type Props = {
  index: SearchEntry[];
};

type SnippetPart = { kind: 'text' | 'mark'; value: string };

type Result = {
  entry: SearchEntry;
  parts: SnippetPart[];
};

const SNIPPET_RADIUS = 100; // ~200 chars total around the match

/**
 * Build a snippet around the first occurrence of `query` in `body`.
 * Returns an array of plain-text / highlight parts — never raw HTML — so
 * React can render it without dangerouslySetInnerHTML.
 */
function buildSnippetParts(body: string, query: string): SnippetPart[] {
  if (!query) return [];
  const lower = body.toLowerCase();
  const q = query.toLowerCase();
  const idx = lower.indexOf(q);

  if (idx === -1) {
    // No literal substring match (fuzzy hit) — show the body head, no highlight.
    const head = body.slice(0, SNIPPET_RADIUS * 2);
    const tail = body.length > head.length ? '…' : '';
    return [{ kind: 'text', value: head + tail }];
  }

  const start = Math.max(0, idx - SNIPPET_RADIUS);
  const end = Math.min(body.length, idx + query.length + SNIPPET_RADIUS);
  const before = body.slice(start, idx);
  const match = body.slice(idx, idx + query.length);
  const after = body.slice(idx + query.length, end);
  const parts: SnippetPart[] = [];
  if (start > 0) parts.push({ kind: 'text', value: '…' });
  parts.push({ kind: 'text', value: before });
  parts.push({ kind: 'mark', value: match });
  parts.push({ kind: 'text', value: after });
  if (end < body.length) parts.push({ kind: 'text', value: '…' });
  return parts;
}

function renderSnippet(parts: SnippetPart[]): ReactNode {
  return parts.map((part, i) =>
    part.kind === 'mark' ? (
      <mark key={i}>{part.value}</mark>
    ) : (
      <Fragment key={i}>{part.value}</Fragment>
    )
  );
}

export function SearchBox({ index }: Props) {
  const [query, setQuery] = useState('');

  const fuse = useMemo(
    () =>
      new Fuse(index, {
        keys: [
          { name: 'title', weight: 0.6 },
          { name: 'body', weight: 0.4 },
        ],
        threshold: 0.4,
        ignoreLocation: true,
        minMatchCharLength: 2,
      }),
    [index]
  );

  const results: Result[] = useMemo(() => {
    const q = query.trim();
    if (!q) return [];
    return fuse.search(q, { limit: 8 }).map((r) => ({
      entry: r.item,
      parts: buildSnippetParts(r.item.body, q),
    }));
  }, [fuse, query]);

  return (
    <div className="search-box">
      <input
        type="search"
        className="search-input"
        placeholder="Search research…"
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        autoComplete="off"
        spellCheck={false}
      />
      {query.trim() && (
        <div className="search-results">
          {results.length === 0 ? (
            <p className="search-empty">No matches.</p>
          ) : (
            results.map(({ entry, parts }) => (
              <Link
                key={entry.slug}
                href={`/${entry.slug}`}
                className="search-result"
                onClick={() => setQuery('')}
              >
                <p className="search-result-title">{entry.title}</p>
                <p className="search-result-snippet">{renderSnippet(parts)}</p>
              </Link>
            ))
          )}
        </div>
      )}
    </div>
  );
}
