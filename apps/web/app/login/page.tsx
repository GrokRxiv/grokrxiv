import { LoginPanel } from "@/components/auth/login-panel";
import { sanitizeNextPath } from "@/lib/auth/server";

type SearchParams = { next?: string };

export default async function LoginPage({
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
