"use client";

import * as React from "react";
import {
  AlertTriangle,
  CheckCircle2,
  CloudUpload,
  FileText,
  Loader2,
  XCircle,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import { Badge, type BadgeProps } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { SampleResponse } from "@/lib/types";

type State =
  | { kind: "idle" }
  | { kind: "uploading"; progress: number }
  | { kind: "done"; result: SampleResponse }
  | { kind: "error"; message: string; hint?: string | null };

const MAX_BYTES = 20 * 1024 * 1024;

export function UploadDropzone() {
  const [state, setState] = React.useState<State>({ kind: "idle" });
  const [dragOver, setDragOver] = React.useState(false);
  const inputRef = React.useRef<HTMLInputElement>(null);

  function handleFile(file: File) {
    if (file.type !== "application/pdf") {
      setState({ kind: "error", message: "Only PDF files are accepted." });
      return;
    }
    if (file.size > MAX_BYTES) {
      setState({ kind: "error", message: "File exceeds 20 MB limit." });
      return;
    }
    upload(file);
  }

  function upload(file: File) {
    setState({ kind: "uploading", progress: 0 });
    const xhr = new XMLHttpRequest();
    const fd = new FormData();
    fd.append("file", file);
    xhr.open("POST", "/api/upload");
    xhr.upload.onprogress = (e) => {
      if (!e.lengthComputable) return;
      const pct = Math.round((e.loaded / e.total) * 100);
      setState({ kind: "uploading", progress: pct });
    };
    xhr.onload = () => {
      try {
        const body = JSON.parse(xhr.responseText) as
          | SampleResponse
          | { error: string; hint?: string | null };
        if (
          xhr.status >= 200 &&
          xhr.status < 300 &&
          "meta_review" in body &&
          body.is_sample === true
        ) {
          setState({ kind: "done", result: body });
        } else {
          const msg = "error" in body ? body.error : `HTTP ${xhr.status}`;
          const hint = "hint" in body ? body.hint ?? null : null;
          setState({ kind: "error", message: msg, hint });
        }
      } catch {
        setState({
          kind: "error",
          message: `Upload failed (HTTP ${xhr.status}).`,
        });
      }
    };
    xhr.onerror = () =>
      setState({
        kind: "error",
        message: "Network error during upload.",
        hint: "Check that the Next.js dev server can reach the network.",
      });
    xhr.send(fd);
  }

  function onDrop(e: React.DragEvent) {
    e.preventDefault();
    setDragOver(false);
    const file = e.dataTransfer.files?.[0];
    if (file) handleFile(file);
  }

  return (
    <div id="upload" className="w-full">
      <div
        onDragOver={(e) => {
          e.preventDefault();
          setDragOver(true);
        }}
        onDragLeave={() => setDragOver(false)}
        onDrop={onDrop}
        className={cn(
          "flex flex-col items-center justify-center rounded-xl border-2 border-dashed border-[color:var(--color-border)] bg-[color:var(--color-card)] p-6 text-center transition-colors sm:p-12",
          dragOver && "border-[color:var(--color-primary)] bg-[color:var(--color-accent)]",
        )}
      >
        {state.kind === "idle" && (
          <>
            <CloudUpload className="mb-3 h-10 w-10 text-[color:var(--color-muted-foreground)]" />
            <h3 className="mb-1 text-lg font-semibold">
              Drop a PDF to generate a sample review
            </h3>
            <p className="mb-4 max-w-md text-sm text-[color:var(--color-muted-foreground)]">
              Fast sample only — not a published GrokRxiv review. Full paper
              reviews require an account and moderation before publication.
            </p>
            <input
              ref={inputRef}
              type="file"
              accept="application/pdf"
              className="hidden"
              onChange={(e) => {
                const file = e.target.files?.[0];
                if (file) handleFile(file);
              }}
            />
            <Button onClick={() => inputRef.current?.click()}>
              Choose PDF
            </Button>
            <p className="mt-3 text-xs text-[color:var(--color-muted-foreground)]">
              Max 20 MB. Same-origin only.
            </p>
          </>
        )}

        {state.kind === "uploading" && (
          <div className="flex w-full max-w-sm flex-col items-center">
            <Loader2 className="mb-3 h-8 w-8 animate-spin text-[color:var(--color-primary)]" />
            <p className="mb-3 text-sm font-medium">
              Generating sample review…
            </p>
            <div className="h-2 w-full overflow-hidden rounded-full bg-[color:var(--color-muted)]">
              <div
                className="h-full bg-[color:var(--color-primary)] transition-all"
                style={{ width: `${state.progress}%` }}
              />
            </div>
            <p className="mt-2 text-xs text-[color:var(--color-muted-foreground)]">
              {state.progress}%
            </p>
          </div>
        )}

        {state.kind === "done" && (
          <DoneView
            result={state.result}
            onReset={() => setState({ kind: "idle" })}
          />
        )}

        {/* error block below */}
        {state.kind === "error" && (
          <div className="flex max-w-xl flex-col items-center">
            <FileText className="mb-3 h-10 w-10 text-[color:var(--color-destructive)]" />
            <h3 className="mb-1 text-lg font-semibold">Upload failed</h3>
            <p className="mb-2 text-sm text-[color:var(--color-destructive)]">
              {state.message}
            </p>
            {state.hint ? (
              <p className="mb-4 max-w-md text-xs text-[color:var(--color-muted-foreground)]">
                {state.hint}
              </p>
            ) : null}
            <Button onClick={() => setState({ kind: "idle" })}>
              Try again
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}

function DoneView({
  result,
  onReset,
}: {
  result: SampleResponse;
  onReset: () => void;
}) {
  const [downloadError, setDownloadError] = React.useState<string | null>(null);
  const tone = sampleTone(result.meta_review.recommendation);
  const StatusIcon = tone.icon;

  const handleDownload = React.useCallback(() => {
    try {
      const bin = atob(result.bundle_b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      const blob = new Blob([bytes], { type: "application/zip" });
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = `grokrxiv-sample-${result.sample_review_id}.zip`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
      setDownloadError(null);
    } catch {
      setDownloadError("Could not prepare the review bundle for download.");
    }
  }, [result.bundle_b64, result.sample_review_id]);

  return (
    <div className="flex w-full max-w-4xl flex-col items-stretch gap-5 text-left">
      <div className="flex flex-col items-center gap-3 text-center">
        <div className="flex flex-wrap items-center justify-center gap-2">
          <StatusIcon className={cn("h-6 w-6", tone.iconClass)} />
          <h3 className="text-lg font-semibold">{tone.heading}</h3>
          <Badge variant={tone.badgeVariant}>{tone.label}</Badge>
        </div>
        <p className="max-w-2xl text-sm leading-6 text-[color:var(--color-muted-foreground)]">
          {result.meta_review.summary}
        </p>
      </div>

      <div className="grid gap-3 rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-background)]/40 p-4 sm:grid-cols-2">
        <div>
          <p className="text-xs font-semibold uppercase tracking-wide text-[color:var(--color-muted-foreground)]">
            Recommendation
          </p>
          <p className="mt-1 text-base font-semibold">{tone.label}</p>
        </div>
        <div>
          <p className="text-xs font-semibold uppercase tracking-wide text-[color:var(--color-muted-foreground)]">
            Confidence
          </p>
          <p className="mt-1 text-base font-semibold">
            {formatConfidence(result.meta_review.confidence)}
          </p>
        </div>
      </div>

      <div className="grid gap-3 lg:grid-cols-3">
        <ReviewList title="Strengths" items={result.meta_review.strengths} />
        <ReviewList title="Weaknesses" items={result.meta_review.weaknesses} />
        <ReviewList title="Questions" items={result.meta_review.questions} />
      </div>

      {downloadError ? (
        <p className="text-center text-sm text-[color:var(--color-destructive)]">
          {downloadError}
        </p>
      ) : null}

      <div className="flex flex-wrap items-center justify-center gap-2">
        <Button type="button" onClick={handleDownload}>
          Download sample review
        </Button>
        <Button type="button" variant="ghost" onClick={onReset}>
          Upload another
        </Button>
      </div>
    </div>
  );
}

type SampleTone = {
  heading: string;
  label: string;
  badgeVariant: BadgeProps["variant"];
  icon: LucideIcon;
  iconClass: string;
};

function sampleTone(
  recommendation: SampleResponse["meta_review"]["recommendation"],
): SampleTone {
  switch (recommendation) {
    case "accept":
      return {
        heading: "Sample ready",
        label: "Accept",
        badgeVariant: "success",
        icon: CheckCircle2,
        iconClass: "text-emerald-500",
      };
    case "minor_revision":
      return {
        heading: "Sample ready",
        label: "Minor revision",
        badgeVariant: "warn",
        icon: AlertTriangle,
        iconClass: "text-amber-500",
      };
    case "major_revision":
      return {
        heading: "Sample needs revision",
        label: "Major revision",
        badgeVariant: "warn",
        icon: AlertTriangle,
        iconClass: "text-amber-500",
      };
    case "reject":
      return {
        heading: "Sample recommends rejection",
        label: "Reject",
        badgeVariant: "destructive",
        icon: XCircle,
        iconClass: "text-[color:var(--color-destructive)]",
      };
  }
}

function formatConfidence(value: number) {
  const bounded = Math.max(0, Math.min(1, value));
  return `${Math.round(bounded * 100)}%`;
}

function ReviewList({ title, items }: { title: string; items: string[] }) {
  const visible = items.filter((item) => item.trim().length > 0);
  return (
    <section className="rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-background)]/40 p-4">
      <h4 className="text-sm font-semibold">{title}</h4>
      {visible.length > 0 ? (
        <ul className="mt-3 space-y-2 text-sm leading-6 text-[color:var(--color-muted-foreground)]">
          {visible.map((item, index) => (
            <li key={`${title}-${index}`} className="flex gap-2">
              <span className="mt-[0.65em] h-1.5 w-1.5 shrink-0 rounded-full bg-[color:var(--color-primary)]" />
              <span>{item}</span>
            </li>
          ))}
        </ul>
      ) : (
        <p className="mt-3 text-sm text-[color:var(--color-muted-foreground)]">
          None noted in this sample.
        </p>
      )}
    </section>
  );
}
