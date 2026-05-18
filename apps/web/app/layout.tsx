import type { Metadata } from "next";
import Script from "next/script";
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
  "GrokRxiv creates structured review reports for arXiv papers. Public reviews are checked and moderated before publication.";
const GA_MEASUREMENT_ID = "G-82HHZNTJYH";

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
  icons: {
    icon: [
      { url: "/brand/grokrxiv-mark.svg", type: "image/svg+xml" },
      { url: "/icon-192.png", sizes: "192x192", type: "image/png" },
      { url: "/icon-512.png", sizes: "512x512", type: "image/png" },
    ],
    shortcut: "/favicon.ico",
    apple: "/apple-touch-icon.png",
  },
};

export default function RootLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  return (
    <html lang="en" suppressHydrationWarning>
      <head>
        {/* Blocking script that applies the saved/system theme to <html>
            before paint. Without this the page renders light at SSR, then
            the client's useEffect adds .dark, causing a visible flicker. */}
        <Script src="/theme-init.js" strategy="beforeInteractive" />
        {process.env.NODE_ENV === "production" ? (
          <>
            <Script
              async
              src={`https://www.googletagmanager.com/gtag/js?id=${GA_MEASUREMENT_ID}`}
              strategy="afterInteractive"
            />
            <Script
              id="google-analytics"
              strategy="afterInteractive"
              dangerouslySetInnerHTML={{
                __html: `
                  window.dataLayer = window.dataLayer || [];
                  function gtag(){dataLayer.push(arguments);}
                  gtag('js', new Date());
                  gtag('config', '${GA_MEASUREMENT_ID}');
                `,
              }}
            />
          </>
        ) : null}
      </head>
      <body className={`${sans.variable} ${mono.variable} font-sans`}>
        <Header />
        <main className="mx-auto w-full max-w-6xl px-4 py-8">{children}</main>
        <Footer />
      </body>
    </html>
  );
}
