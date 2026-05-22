import { z } from "zod";
import { createSupabaseServiceClient } from "@/lib/supabase/service";
import { adminUsersRedirect, requireAdminUser } from "../../_lib";

const Body = z.object({
  plan_id: z.enum(["free", "supporter", "researcher", "admin"]),
  status: z.enum(["active", "trialing", "past_due", "canceled"]).default("active"),
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
    return adminUsersRedirect(request, "bad_billing");
  }

  const supabase = createSupabaseServiceClient();
  await supabase.rpc("grokrxiv_ensure_user_account", {
    target_user_id: userId,
    target_email: null,
  });
  const now = new Date().toISOString();
  const { error: billingError } = await supabase.from("user_billing").upsert(
    {
      user_id: userId,
      plan_id: parsed.data.plan_id,
      status: parsed.data.status,
      provider: "manual",
      updated_at: now,
    },
    { onConflict: "user_id" },
  );
  const { error: profileError } = await supabase
    .from("profiles")
    .update({ billing_tier: parsed.data.plan_id, updated_at: now })
    .eq("user_id", userId);

  if (billingError || profileError) {
    return adminUsersRedirect(request, "billing_update_failed");
  }
  return adminUsersRedirect(request, "billing_updated");
}
