'use client';

import Link from 'next/link';
import { usePathname } from 'next/navigation';
import type { ResearchDoc } from '@/lib/research-docs';
import type { SearchEntry } from '@/lib/search';
import { SearchBox } from './search-box';

type Props = {
  docs: ResearchDoc[];
  searchIndex: SearchEntry[];
};

export function Sidebar({ docs, searchIndex }: Props) {
  const pathname = usePathname();
  // pathname is `/` for the index, `/{slug}` for a doc viewer.
  const currentSlug = pathname && pathname !== '/' ? pathname.slice(1) : null;

  return (
    <>
      <p className="sidebar-header">GrokRxiv</p>
      <h1 className="sidebar-title">
        <Link href="/" className="sidebar-link" style={{ padding: 0, display: 'inline' }}>
          Research
        </Link>
      </h1>

      <SearchBox index={searchIndex} />

      <p className="sidebar-section">Documents</p>
      <ul className="sidebar-list">
        {docs.length === 0 ? (
          <li className="search-empty">No HTML artifacts in research/.</li>
        ) : (
          docs.map((doc) => {
            const isActive = doc.slug === currentSlug;
            return (
              <li key={doc.slug}>
                <Link
                  href={`/${doc.slug}`}
                  className={`sidebar-link ${isActive ? 'active' : ''}`}
                >
                  {doc.title}
                </Link>
              </li>
            );
          })
        )}
      </ul>
    </>
  );
}
