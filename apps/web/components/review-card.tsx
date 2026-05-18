import Link from "next/link";
import {
  Card,
  CardContent,
  CardDescription,
  CardFooter,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { ReviewStatusBadge } from "@/components/review-status-badge";
import { MathText } from "@/components/math-text";
import type { ReviewWithPaper } from "@/lib/types";

export function ReviewCard({ review }: { review: ReviewWithPaper }) {
  const { paper, meta_review } = review;
  return (
    <Link href={`/reviews/${review.id}`} className="block h-full">
      <Card className="h-full transition-shadow hover:shadow-md">
        <CardHeader>
          <div className="flex items-center justify-between gap-2">
            <Badge variant="outline">{paper.field ?? "—"}</Badge>
            <ReviewStatusBadge status={review.status} />
          </div>
          <CardTitle className="line-clamp-3 text-base">
            <MathText as="span">{paper.title}</MathText>
          </CardTitle>
          <CardDescription className="line-clamp-1">
            {paper.authors
              .slice(0, 3)
              .map((a) => a.name)
              .join(", ")}
            {paper.authors.length > 3 ? " et al." : ""}
          </CardDescription>
        </CardHeader>
        <CardContent>
          <p className="line-clamp-3 text-sm text-[color:var(--color-muted-foreground)]">
            {meta_review?.summary ?? paper.abstract ?? ""}
          </p>
        </CardContent>
        <CardFooter className="flex items-center justify-between text-xs text-[color:var(--color-muted-foreground)]">
          <span className="font-mono">{paper.arxiv_id}</span>
          {meta_review?.recommendation ? (
            <Badge variant="secondary">{meta_review.recommendation}</Badge>
          ) : null}
        </CardFooter>
      </Card>
    </Link>
  );
}
