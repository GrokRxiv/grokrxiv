import type { Metadata } from "next";
import { Inter, JetBrains_Mono } from "next/font/google";
import { Header } from "@/components/header";
import { Footer } from "@/components/footer";
import { CANONICAL_URL } from "@/lib/env";
import "./globals.css";

const sans = Inter({
  subsets: ["latin"],
  variable: "--font-geist-sans",
  display: "swap",
});
const mono = JetBrains_Mono({
  subsets: ["latin"],
  variable: "--font-geist-mono",
  display: "swap",
});

const SITE_BLURB =
  "GrokRxiv is an agentic peer-review system that automates the review → revise → publish pipeline for arXiv papers. Six specialist LLM reviewers run under a typed verifier ladder; every review ships as a human-gated PR.";

export const metadata: Metadata = {
  metadataBase: new URL(CANONICAL_URL),
  title: {
    default: "GrokRxiv — Agentic peer review for arXiv",
    template: "%s · GrokRxiv",
  },
  description: SITE_BLURB,
  alternates: { canonical: "/" },
  openGraph: {
    type: "website",
    url: CANONICAL_URL,
    siteName: "GrokRxiv",
    title: "GrokRxiv — Agentic peer review for arXiv",
    description: SITE_BLURB,
  },
  twitter: {
    card: "summary_large_image",
    title: "GrokRxiv — Agentic peer review for arXiv",
    description: SITE_BLURB,
  },
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" suppressHydrationWarning>
      <body className={`${sans.variable} ${mono.variable} font-sans`}>
        <Header />
        <main className="mx-auto w-full max-w-6xl px-4 py-8">{children}</main>
        <Footer />
      </body>
    </html>
  );
}
