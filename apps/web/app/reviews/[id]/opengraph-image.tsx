import { ImageResponse } from "next/og";
import { getPaperByIdAnon, getReviewByIdAnon } from "@/lib/supabase/anon";

export const alt = "GrokRxiv review";
export const size = { width: 1200, height: 630 };
export const contentType = "image/png";

export default async function OgImage({
  params,
}: {
  params: { id: string };
}) {
  const review = await getReviewByIdAnon(params.id);
  let title = "GrokRxiv review";
  let field = "arXiv";
  if (review) {
    const paper = await getPaperByIdAnon(review.paper_id);
    if (paper) {
      title = paper.title;
      field = paper.field ?? "arXiv";
    }
  }

  return new ImageResponse(
    (
      <div
        style={{
          width: "100%",
          height: "100%",
          display: "flex",
          flexDirection: "column",
          justifyContent: "space-between",
          padding: 64,
          background: "linear-gradient(135deg, #0f172a 0%, #1e293b 100%)",
          color: "white",
          fontFamily: "sans-serif",
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 16 }}>
          <div
            style={{
              fontSize: 36,
              fontWeight: 800,
              letterSpacing: -1,
              color: "white",
            }}
          >
            GrokRxiv
          </div>
          <div
            style={{
              padding: "6px 12px",
              borderRadius: 999,
              background: "rgba(255,255,255,0.1)",
              fontSize: 18,
              color: "#cbd5e1",
            }}
          >
            {field}
          </div>
        </div>
        <div
          style={{
            fontSize: 56,
            fontWeight: 700,
            lineHeight: 1.15,
            color: "white",
            maxWidth: 1000,
          }}
        >
          {title.length > 140 ? `${title.slice(0, 137)}…` : title}
        </div>
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            fontSize: 20,
            color: "#94a3b8",
          }}
        >
          <span>Public GrokRxiv review</span>
          <span>grokrxiv.org</span>
        </div>
      </div>
    ),
    size,
  );
}
