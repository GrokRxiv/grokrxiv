import Link from 'next/link';
import { notFound } from 'next/navigation';
import { listDocs, readDocHtml } from '@/lib/research-docs';

export const dynamic = 'force-dynamic';

// Pre-enumerate slugs at build time so each doc gets a known route. New files
// added after build are still served because the route uses dynamic = 'force-dynamic'.
export async function generateStaticParams() {
  const docs = await listDocs();
  return docs.map((doc) => ({ slug: doc.slug }));
}

type Params = { slug: string };

export async function generateMetadata({ params }: { params: Promise<Params> }) {
  const { slug } = await params;
  const docs = await listDocs();
  const doc = docs.find((d) => d.slug === slug);
  return {
    title: doc ? `${doc.title} — GrokRxiv Research` : 'Research — GrokRxiv',
  };
}

export default async function DocPage({ params }: { params: Promise<Params> }) {
  const { slug } = await params;
  const html = await readDocHtml(slug);
  if (!html) {
    notFound();
  }

  const docs = await listDocs();
  const doc = docs.find((d) => d.slug === slug);
  const title = doc?.title ?? slug;

  return (
    <>
      <p className="breadcrumb">
        <Link href="/">All research</Link>
        <span aria-hidden>/</span>
        <span className="current">{title}</span>
      </p>
      {/*
        srcDoc + sandbox="allow-same-origin" embeds the full HTML document
        while isolating its scripts from this app's origin. Iframe sandboxing
        is the safer pattern for fully-formed HTML we generated ourselves.
      */}
      <iframe
        srcDoc={html}
        sandbox="allow-same-origin"
        title={title}
        style={{
          width: '100%',
          height: 'calc(100vh - 80px)',
          border: 0,
          background: '#0d1117',
        }}
      />
    </>
  );
}
