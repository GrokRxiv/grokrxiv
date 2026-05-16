"use client";

import * as React from "react";
import { CloudUpload, FileText, Loader2, CheckCircle2 } from "lucide-react";
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
              Fast single-pass sample — not a published GrokRxiv review. Full
              six-agent reviews run automatically on newly ingested arXiv
              papers and land in moderation before publication.
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
  // Decode the base64 zip once on mount and turn it into a Blob URL so the
  // browser handles the download as a real file instead of a data: URL (which
  // some browsers truncate at megabyte scale).
  const bundleUrl = React.useMemo(() => {
    try {
      const bin = atob(result.bundle_b64);
      const bytes = new Uint8Array(bin.length);
      for (let i = 0; i < bin.length; i++) bytes[i] = bin.charCodeAt(i);
      const blob = new Blob([bytes], { type: "application/zip" });
      return URL.createObjectURL(blob);
    } catch {
      return "";
    }
  }, [result.bundle_b64]);

  React.useEffect(() => {
    return () => {
      if (bundleUrl) URL.revokeObjectURL(bundleUrl);
    };
  }, [bundleUrl]);

  return (
    <div className="flex w-full max-w-3xl flex-col items-stretch gap-4">
      <div className="flex items-center justify-center gap-2">
        <CheckCircle2 className="h-6 w-6 text-emerald-500" />
        <h3 className="text-lg font-semibold">Sample ready</h3>
      </div>
      <p className="mx-auto max-w-2xl text-sm text-[color:var(--color-muted-foreground)]">
        {result.meta_review.summary}
      </p>
      <iframe
        title="Sample review preview"
        srcDoc={result.html}
        sandbox="allow-same-origin"
        className="aspect-[4/5] min-h-[320px] w-full rounded-md border border-[color:var(--color-border)] bg-white sm:aspect-auto sm:h-[480px]"
      />
      <div className="flex flex-wrap items-center justify-center gap-2">
        <Button asChild>
          <a
            href={bundleUrl}
            download={`grokrxiv-sample-${result.sample_review_id}.zip`}
          >
            Download bundle.zip
          </a>
        </Button>
        <Button variant="ghost" onClick={onReset}>
          Upload another
        </Button>
      </div>
    </div>
  );
}
