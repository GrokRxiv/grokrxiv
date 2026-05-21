import { NextResponse } from "next/server";
import { z } from "zod";
import { getCurrentUser } from "@/lib/auth/server";
import {
  billingConfigured,
  priceIdForPlan,
  stripeCreateCheckoutSession,
  stripeCreateCustomer,
} from "@/lib/billing";
import { createSupabaseServiceClient } from "@/lib/supabase/service";

const Body = z.object({
  plan: z.enum(["supporter", "researcher"]),
});

export async function POST(request: Request) {
  const url = new URL(request.url);
  const { user } = await getCurrentUser();
  if (!user) {
    return NextResponse.redirect(new URL("/login?next=/pricing", url), {
      status: 303,
    });
  }

  const form = await request.formData().catch(() => null);
  const parsed = Body.safeParse(form ? Object.fromEntries(form) : {});
  if (!parsed.success) {
    return NextResponse.json({ error: "bad_plan" }, { status: 400 });
  }

  const priceId = priceIdForPlan(parsed.data.plan);
  if (!billingConfigured() || !priceId) {
    return NextResponse.redirect(new URL("/pricing?billing=disabled", url), {
      status: 303,
    });
  }

  const supabase = createSupabaseServiceClient();
  await supabase.rpc("grokrxiv_ensure_user_account", {
    target_user_id: user.id,
    target_email: user.email ?? null,
  });

  const customerId = await ensureStripeCustomer(supabase, {
    userId: user.id,
    email: user.email ?? null,
  });
  const session = await stripeCreateCheckoutSession({
    customerId,
    userId: user.id,
    planId: parsed.data.plan,
    priceId,
  });

  if (!session.url) {
    return NextResponse.json(
      { error: "checkout_session_missing_url" },
      { status: 502 },
    );
  }
  return NextResponse.redirect(session.url, { status: 303 });
}

async function ensureStripeCustomer(
  supabase: ReturnType<typeof createSupabaseServiceClient>,
  user: { userId: string; email: string | null },
): Promise<string> {
  const { data } = await supabase
    .from("user_billing")
    .select("stripe_customer_id")
    .eq("user_id", user.userId)
    .maybeSingle();
  if (typeof data?.stripe_customer_id === "string" && data.stripe_customer_id) {
    return data.stripe_customer_id;
  }

  const customer = await stripeCreateCustomer(user);
  await supabase
    .from("user_billing")
    .update({
      provider: "stripe",
      stripe_customer_id: customer.id,
      updated_at: new Date().toISOString(),
    })
    .eq("user_id", user.userId);
  return customer.id;
}
