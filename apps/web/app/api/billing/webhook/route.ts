import { NextResponse } from "next/server";
import {
  idOf,
  normalizeStripeStatus,
  planForPriceId,
  stripeRetrieveSubscription,
  stripeSubscriptionItems,
  type StripeCheckoutSession,
  type StripeEvent,
  type StripeSubscription,
  unixToIso,
  verifyStripeWebhook,
} from "@/lib/billing";
import { createSupabaseServiceClient } from "@/lib/supabase/service";

export async function POST(request: Request) {
  const signature = request.headers.get("stripe-signature");
  if (!signature) {
    return NextResponse.json({ error: "missing_signature" }, { status: 400 });
  }

  const rawBody = await request.text();
  let event: StripeEvent;
  try {
    event = verifyStripeWebhook(rawBody, signature);
  } catch (error) {
    const message = error instanceof Error ? error.message : "Invalid signature.";
    return NextResponse.json(
      { error: "bad_signature", detail: message },
      { status: 400 },
    );
  }

  const supabase = createSupabaseServiceClient();
  if (event.type === "checkout.session.completed") {
    await handleCheckoutCompleted(
      supabase,
      event.data?.object as StripeCheckoutSession | undefined,
    );
  } else if (
    event.type === "customer.subscription.created" ||
    event.type === "customer.subscription.updated" ||
    event.type === "customer.subscription.deleted"
  ) {
    await syncSubscription(
      supabase,
      event.data?.object as StripeSubscription | undefined,
    );
  }

  const recorded = await recordEvent(supabase, event);
  return NextResponse.json({ received: true, duplicate: !recorded });
}

async function recordEvent(
  supabase: ReturnType<typeof createSupabaseServiceClient>,
  event: StripeEvent,
): Promise<boolean> {
  const { error } = await supabase.from("stripe_webhook_events").insert({
    id: event.id,
    event_type: event.type,
    payload: event as unknown as Record<string, unknown>,
  });
  if (!error) return true;
  if (error.code === "23505") return false;
  throw new Error(error.message);
}

async function handleCheckoutCompleted(
  supabase: ReturnType<typeof createSupabaseServiceClient>,
  session: StripeCheckoutSession | undefined,
) {
  if (!session) return;
  const userId = session.metadata?.user_id ?? session.client_reference_id;
  const customerId = idOf(session.customer);
  if (!userId) return;
  if (customerId) {
    await supabase
      .from("user_billing")
      .update({
        provider: "stripe",
        stripe_customer_id: customerId,
        updated_at: new Date().toISOString(),
      })
      .eq("user_id", userId);
  }

  const subscriptionId = idOf(session.subscription);
  if (subscriptionId) {
    await syncSubscription(supabase, await stripeRetrieveSubscription(subscriptionId));
  }
}

async function syncSubscription(
  supabase: ReturnType<typeof createSupabaseServiceClient>,
  subscription: StripeSubscription | undefined,
) {
  if (!subscription) return;
  const customerId = idOf(subscription.customer);
  const subscriptionId = subscription.id;
  const firstItem = stripeSubscriptionItems(subscription)[0];
  const priceId = firstItem?.price?.id ?? null;
  const activePlan = planForPriceId(priceId);
  const userId =
    subscription.metadata?.user_id ??
    (await findUserIdForSubscription(supabase, subscriptionId, customerId));
  if (!userId) return;

  const status = normalizeStripeStatus(subscription.status);
  const planId = status === "canceled" ? "free" : activePlan ?? "free";
  await supabase.from("user_billing").upsert(
    {
      user_id: userId,
      plan_id: planId,
      status,
      provider: "stripe",
      period_start: unixToIso(firstItem?.current_period_start),
      period_end: unixToIso(firstItem?.current_period_end),
      stripe_customer_id: customerId,
      stripe_subscription_id: subscriptionId,
      stripe_price_id: priceId,
      updated_at: new Date().toISOString(),
    },
    { onConflict: "user_id" },
  );
  await supabase
    .from("profiles")
    .update({
      billing_tier: planId,
      updated_at: new Date().toISOString(),
    })
    .eq("user_id", userId);
}

async function findUserIdForSubscription(
  supabase: ReturnType<typeof createSupabaseServiceClient>,
  subscriptionId: string,
  customerId: string | null,
): Promise<string | null> {
  let { data } = await supabase
    .from("user_billing")
    .select("user_id")
    .eq("stripe_subscription_id", subscriptionId)
    .maybeSingle();
  if (typeof data?.user_id === "string") return data.user_id;
  if (!customerId) return null;
  ({ data } = await supabase
    .from("user_billing")
    .select("user_id")
    .eq("stripe_customer_id", customerId)
    .maybeSingle());
  return typeof data?.user_id === "string" ? data.user_id : null;
}
