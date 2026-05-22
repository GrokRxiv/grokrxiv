import { z } from "zod";
import { createSupabaseServiceClient } from "@/lib/supabase/service";
import { adminUsersRedirect, requireAdminUser } from "../../_lib";

const Body = z.object({
  review_limit_override: z.string().max(8).optional(),
});

export async function POST(
  request: Request,
  { params }: { params: Promise<{ userId: string }> },
) {
  const access = await requireAdminUser(request);
  if (!access.ok) return access.response;

  const { userId } = await params;
  const form = await request.formData().catch(() => null);
  const parsed = Body.safeParse(form ? Object.fromEntries(form) : {});
  if (!parsed.success) {
    return adminUsersRedirect(request, "bad_quota");
  }

  const raw = parsed.data.review_limit_override?.trim() ?? "";
  const override = raw.length === 0 ? null : Number.parseInt(raw, 10);
  if (override !== null && (!Number.isFinite(override) || override < 0)) {
    return adminUsersRedirect(request, "bad_quota");
  }

  const supabase = createSupabaseServiceClient();
  const { error } = await supabase
    .from("profiles")
    .update({
      review_limit_override: override,
      updated_at: new Date().toISOString(),
    })
    .eq("user_id", userId);
  if (error) {
    return adminUsersRedirect(request, "quota_update_failed");
  }
  return adminUsersRedirect(request, "quota_updated");
}
