import type { MetadataRoute } from "next";
import {
  listAllPaperArxivIdsAnon,
  listAllPublishedReviewIdsAnon,
} from "@/lib/supabase/anon";
import { CANONICAL_URL } from "@/lib/env";

export default async function sitemap(): Promise<MetadataRoute.Sitemap> {
  "use cache";
  const base: MetadataRoute.Sitemap = [
    { url: `${CANONICAL_URL}/`, changeFrequency: "hourly", priority: 1 },
    { url: `${CANONICAL_URL}/reviews`, changeFrequency: "hourly", priority: 0.9 },
    { url: `${CANONICAL_URL}/about`, changeFrequency: "monthly", priority: 0.6 },
    { url: `${CANONICAL_URL}/api-docs`, changeFrequency: "monthly", priority: 0.5 },
    { url: `${CANONICAL_URL}/legal`, changeFrequency: "yearly", priority: 0.3 },
  ];

  const [reviews, papers] = await Promise.all([
    listAllPublishedReviewIdsAnon(),
    listAllPaperArxivIdsAnon(),
  ]);

  for (const r of reviews) {
    base.push({
      url: `${CANONICAL_URL}/reviews/${r.id}`,
      lastModified: r.published_at ? new Date(r.published_at) : undefined,
      changeFrequency: "weekly",
      priority: 0.8,
    });
  }
  for (const p of papers) {
    base.push({
      url: `${CANONICAL_URL}/papers/${p.arxiv_id}`,
      lastModified: new Date(p.ingested_at),
      changeFrequency: "weekly",
      priority: 0.6,
    });
  }

  return base;
}
