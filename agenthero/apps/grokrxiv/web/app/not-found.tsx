import Link from "next/link";

export default function NotFound() {
  return (
    <div className="flex min-h-[50vh] flex-col items-center justify-center gap-4 text-center">
      <h1 className="text-4xl font-bold">404</h1>
      <p className="text-[color:var(--color-muted-foreground)]">
        We couldn&apos;t find that page.
      </p>
      <Link href="/" className="underline underline-offset-4">
        Back to GrokRxiv
      </Link>
    </div>
  );
}
