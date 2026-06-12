import crypto from "node:crypto";
import {
  GROKRXIV_BILLING_ENABLED,
  SITE_URL,
  STRIPE_RESEARCHER_PRICE_ID,
  STRIPE_SECRET_KEY,
  STRIPE_SUPPORTER_PRICE_ID,
  STRIPE_WEBHOOK_SECRET,
} from "@/lib/env";

export type PaidPlanId = "supporter" | "researcher";
export type BillingPlanId = "free" | PaidPlanId | "admin";

export type StripeObjectRef = string | { id?: string } | null | undefined;
export type StripeEvent = {
  id: string;
  type: string;
  data?: { object?: Record<string, unknown> };
};
export type StripeCheckoutSession = Record<string, unknown> & {
  client_reference_id?: string | null;
  customer?: StripeObjectRef;
  metadata?: Record<string, string>;
  subscription?: StripeObjectRef;
};
export type StripeSubscription = Record<string, unknown> & {
  id: string;
  status: string;
  customer?: StripeObjectRef;
  metadata?: Record<string, string>;
  items?: { data?: StripeSubscriptionItem[] };
};
export type StripeSubscriptionItem = {
  price?: { id?: string };
  current_period_start?: number;
  current_period_end?: number;
};

const TRUTHY = new Set(["1", "true", "yes", "on"]);
const STRIPE_API_BASE = "https://api.stripe.com/v1";

export function billingEnabledFlag(): boolean {
  return TRUTHY.has(GROKRXIV_BILLING_ENABLED.trim().toLowerCase());
}

export function billingConfigured(): boolean {
  return (
    billingEnabledFlag() &&
    STRIPE_SECRET_KEY.length > 0 &&
    STRIPE_WEBHOOK_SECRET.length > 0 &&
    STRIPE_SUPPORTER_PRICE_ID.length > 0 &&
    STRIPE_RESEARCHER_PRICE_ID.length > 0
  );
}

export function priceIdForPlan(plan: PaidPlanId): string {
  return plan === "supporter"
    ? STRIPE_SUPPORTER_PRICE_ID
    : STRIPE_RESEARCHER_PRICE_ID;
}

export function planForPriceId(priceId: string | null | undefined): PaidPlanId | null {
  if (priceId && priceId === STRIPE_SUPPORTER_PRICE_ID) return "supporter";
  if (priceId && priceId === STRIPE_RESEARCHER_PRICE_ID) return "researcher";
  return null;
}

export function normalizeStripeStatus(
  status: string,
): "active" | "trialing" | "past_due" | "canceled" {
  switch (status) {
    case "active":
    case "trialing":
    case "past_due":
    case "canceled":
      return status;
    default:
      return "canceled";
  }
}

export function unixToIso(value: number | null | undefined): string | null {
  return value ? new Date(value * 1000).toISOString() : null;
}

export function idOf(value: StripeObjectRef): string | null {
  if (typeof value === "string") return value;
  return typeof value?.id === "string" ? value.id : null;
}

export async function stripeCreateCustomer(input: {
  email: string | null;
  userId: string;
}): Promise<{ id: string }> {
  const body = new URLSearchParams();
  if (input.email) body.set("email", input.email);
  body.set("metadata[user_id]", input.userId);
  return stripeRequest("POST", "/customers", body);
}

export async function stripeCreateCheckoutSession(input: {
  customerId: string;
  userId: string;
  planId: PaidPlanId;
  priceId: string;
}): Promise<{ url?: string | null }> {
  const body = new URLSearchParams();
  body.set("mode", "subscription");
  body.set("customer", input.customerId);
  body.set("client_reference_id", input.userId);
  body.set("line_items[0][price]", input.priceId);
  body.set("line_items[0][quantity]", "1");
  body.set("success_url", `${SITE_URL}/dashboard?billing=success`);
  body.set("cancel_url", `${SITE_URL}/pricing?billing=cancelled`);
  body.set("allow_promotion_codes", "true");
  body.set("metadata[user_id]", input.userId);
  body.set("metadata[plan_id]", input.planId);
  body.set("subscription_data[metadata][user_id]", input.userId);
  body.set("subscription_data[metadata][plan_id]", input.planId);
  return stripeRequest("POST", "/checkout/sessions", body);
}

export async function stripeCreatePortalSession(input: {
  customerId: string;
}): Promise<{ url: string }> {
  const body = new URLSearchParams();
  body.set("customer", input.customerId);
  body.set("return_url", `${SITE_URL}/dashboard`);
  return stripeRequest("POST", "/billing_portal/sessions", body);
}

export async function stripeRetrieveSubscription(
  subscriptionId: string,
): Promise<StripeSubscription> {
  return stripeRequest("GET", `/subscriptions/${encodeURIComponent(subscriptionId)}`);
}

export function verifyStripeWebhook(rawBody: string, signature: string): StripeEvent {
  const parts = Object.fromEntries(
    signature.split(",").map((part) => {
      const [key, value] = part.split("=", 2);
      return [key, value];
    }),
  );
  const timestamp = parts.t;
  const expected = parts.v1;
  if (!timestamp || !expected) throw new Error("Malformed Stripe signature.");
  const actual = crypto
    .createHmac("sha256", STRIPE_WEBHOOK_SECRET)
    .update(`${timestamp}.${rawBody}`)
    .digest("hex");
  const actualBytes = Buffer.from(actual, "hex");
  const expectedBytes = Buffer.from(expected, "hex");
  if (
    actualBytes.length !== expectedBytes.length ||
    !crypto.timingSafeEqual(actualBytes, expectedBytes)
  ) {
    throw new Error("Invalid Stripe signature.");
  }
  return JSON.parse(rawBody) as StripeEvent;
}

export function stripeSubscriptionItems(
  subscription: StripeSubscription,
): StripeSubscriptionItem[] {
  return subscription.items?.data ?? [];
}

async function stripeRequest<T>(
  method: "GET" | "POST",
  path: string,
  body?: URLSearchParams,
): Promise<T> {
  const response = await fetch(`${STRIPE_API_BASE}${path}`, {
    method,
    headers: {
      authorization: `Bearer ${STRIPE_SECRET_KEY}`,
      ...(body ? { "content-type": "application/x-www-form-urlencoded" } : {}),
    },
    body,
  });
  const text = await response.text();
  const payload = text ? JSON.parse(text) : {};
  if (!response.ok) {
    const message =
      typeof payload?.error?.message === "string"
        ? payload.error.message
        : `Stripe API returned ${response.status}`;
    throw new Error(message);
  }
  return payload as T;
}
