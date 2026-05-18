import { Suspense } from "react";
import { LoginPanel } from "@/components/auth/login-panel";
import { sanitizeNextPath } from "@/lib/auth/server";

type SearchParams = { next?: string };

export default function LoginPage({
  searchParams,
}: {
  searchParams: Promise<SearchParams>;
}) {
  return (
    <Suspense
      fallback={
        <div className="flex min-h-[60vh] items-center justify-center py-10 text-sm text-[color:var(--color-muted-foreground)]">
          Loading sign in...
        </div>
      }
    >
      <LoginPageContent searchParams={searchParams} />
    </Suspense>
  );
}

async function LoginPageContent({
  searchParams,
}: {
  searchParams: Promise<SearchParams>;
}) {
  const { next } = await searchParams;
  return (
    <div className="flex min-h-[60vh] items-center justify-center py-10">
      <LoginPanel nextPath={sanitizeNextPath(next)} />
    </div>
  );
}
