import Link from 'next/link';
import { listDocs } from '@/lib/research-docs';

export const dynamic = 'force-dynamic';

function formatDate(iso: string): string {
  try {
    const d = new Date(iso);
    return d.toLocaleString(undefined, {
      year: 'numeric',
      month: 'short',
      day: '2-digit',
      hour: '2-digit',
      minute: '2-digit',
    });
  } catch {
    return iso;
  }
}

function formatWordCount(n: number): string {
  if (n < 1000) return `${n} words`;
  return `${(n / 1000).toFixed(1)}k words`;
}

export default async function HomePage() {
  const docs = await listDocs();

  return (
    <>
      <p className="breadcrumb">
        <span className="current">All research</span>
      </p>

      {docs.length === 0 ? (
        <div className="empty-state">
          No research artifacts found in <code>research/*.html</code>.
          <br />
          Run the Track B build script to generate them.
        </div>
      ) : (
        <ul className="index-list" style={{ listStyle: 'none', margin: 0, padding: 0 }}>
          {docs.map((doc) => (
            <li key={doc.slug} className="index-item">
              <h2>
                <Link href={`/${doc.slug}`}>{doc.title}</Link>
              </h2>
              {doc.summary && <p>{doc.summary}</p>}
              <div className="index-meta">
                <span>{formatWordCount(doc.wordCount)}</span>
                <span>updated {formatDate(doc.mtimeISO)}</span>
                <span>{doc.slug}.html</span>
              </div>
            </li>
          ))}
        </ul>
      )}
    </>
  );
}
