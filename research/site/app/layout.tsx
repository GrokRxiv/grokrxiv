import type { Metadata } from 'next';
import type { ReactNode } from 'react';
import './globals.css';
import { Sidebar } from '@/components/sidebar';
import { listDocs } from '@/lib/research-docs';
import { buildSearchIndex } from '@/lib/search';

export const metadata: Metadata = {
  title: 'GrokRxiv Research',
  description: 'Local viewer for generated research artifacts.',
};

// Filesystem state changes whenever Track B rebuilds an HTML file, so opt out
// of caching the doc list / search index at the layout level.
export const dynamic = 'force-dynamic';

export default async function RootLayout({ children }: { children: ReactNode }) {
  const [docs, searchIndex] = await Promise.all([listDocs(), buildSearchIndex()]);

  return (
    <html lang="en">
      <body>
        <div className="shell">
          <aside className="sidebar">
            <Sidebar docs={docs} searchIndex={searchIndex} />
          </aside>
          <main className="content">{children}</main>
        </div>
      </body>
    </html>
  );
}
