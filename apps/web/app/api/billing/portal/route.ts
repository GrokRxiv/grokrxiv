import { NextResponse } from "next/server";
import { getCurrentUser } from "@/lib/auth/server";
import { billingConfigured, stripeCreatePortalSession } from "@/lib/billing";
import { createSupabaseServiceClient } from "@/lib/supabase/service";

export async function POST(request: Request) {
  const url = new URL(request.url);
  const { user } = await getCurrentUser();
  if (!user) {
    return NextResponse.redirect(new URL("/login?next=/dashboard", url), {
      status: 303,
    });
  }

  if (!billingConfigured()) {
    return NextResponse.redirect(new URL("/pricing?billing=disabled", url), {
      status: 303,
    });
  }

  const supabase = createSupabaseServiceClient();
  const { data } = await supabase
    .from("user_billing")
    .select("stripe_customer_id")
    .eq("user_id", user.id)
    .maybeSingle();
  const customerId = data?.stripe_customer_id;
  if (typeof customerId !== "string" || customerId.length === 0) {
    return NextResponse.redirect(new URL("/pricing", url), { status: 303 });
  }

  const session = await stripeCreatePortalSession({ customerId });
  return NextResponse.redirect(session.url, { status: 303 });
}
