import Link from "next/link";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

const TIERS = [
  {
    name: "Anonymous",
    price: "Free",
    badge: "Sample only",
    description:
      "Quick PDF preview for trying the system without creating an account.",
    points: [
      "Sample PDF review only",
      "No full paper review",
      "No saved public or private review",
      "Rate-limited preview queue",
    ],
    cta: "Try sample preview",
    href: "/#sample-review",
  },
  {
    name: "Free account",
    price: "$0",
    badge: "Public",
    description:
      "A capped full-review tier that contributes to the public GrokRxiv corpus.",
    points: [
      "3 lifetime full reviews",
      "Public reviews only",
      "Standard review queue",
      "No surprise usage charges",
    ],
    cta: "Create account",
    href: "/login?next=/dashboard",
  },
  {
    name: "Supporter",
    price: "$5/mo",
    badge: "Planned",
    description:
      "Affordable quota for regular readers who need some private work.",
    points: [
      "10 public reviews per month",
      "2 private reviews per month",
      "Higher queue priority than free",
      "No surprise usage charges",
    ],
    cta: "View dashboard",
    href: "/dashboard",
  },
  {
    name: "Researcher",
    price: "$15/mo",
    badge: "Planned",
    description:
      "More monthly quota for labs, authors, and repeat paper triage.",
    points: [
      "30 public reviews per month",
      "10 private reviews per month",
      "Private dashboard access",
      "Optional extra review credits",
    ],
    cta: "View dashboard",
    href: "/dashboard",
  },
] as const;

export default function PricingPage() {
  return (
    <div className="flex flex-col gap-10 py-8">
      <header className="flex max-w-3xl flex-col gap-4">
        <p className="font-mono text-xs uppercase tracking-widest text-[color:var(--color-muted-foreground)]">
          Pricing and quotas
        </p>
        <h1 className="text-4xl font-bold tracking-tight">
          Public reviews stay cheap. Private reviews pay for capacity.
        </h1>
        <p className="text-lg text-[color:var(--color-muted-foreground)]">
          GrokRxiv is priced around hard review caps, not unlimited monthly
          usage. Free full reviews are public; paid tiers add quota and private
          review access. Any extra-cost rerun is confirmed before it starts.
        </p>
      </header>

      <section className="grid gap-4 md:grid-cols-2 xl:grid-cols-4">
        {TIERS.map((tier) => (
          <Card key={tier.name} className="flex flex-col">
            <CardHeader>
              <div className="flex items-center justify-between gap-3">
                <CardTitle>{tier.name}</CardTitle>
                <Badge variant="secondary">{tier.badge}</Badge>
              </div>
              <CardDescription>{tier.description}</CardDescription>
            </CardHeader>
            <CardContent className="flex flex-1 flex-col gap-5">
              <p className="text-3xl font-bold">{tier.price}</p>
              <ul className="flex flex-col gap-2 text-sm text-[color:var(--color-muted-foreground)]">
                {tier.points.map((point) => (
                  <li key={point} className="flex gap-2">
                    <span aria-hidden="true">-</span>
                    <span>{point}</span>
                  </li>
                ))}
              </ul>
              <Button asChild variant="outline" className="mt-auto">
                <Link href={tier.href}>{tier.cta}</Link>
              </Button>
            </CardContent>
          </Card>
        ))}
      </section>

      <section className="rounded-lg border border-[color:var(--color-border)] p-6">
        <h2 className="text-2xl font-semibold tracking-tight">
          No Surprise Charges
        </h2>
        <p className="mt-3 max-w-3xl text-sm text-[color:var(--color-muted-foreground)]">
          Review limits are enforced before a full job starts. Free reviews are
          public and capped; private reviews require a paid plan. Any premium
          rerun that could use extra paid capacity must be confirmed up front.
        </p>
      </section>
    </div>
  );
}
