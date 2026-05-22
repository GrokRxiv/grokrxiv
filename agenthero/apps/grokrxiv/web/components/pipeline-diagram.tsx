import { ArrowRight, FileText, GitPullRequest, Workflow } from "lucide-react";

const STEPS = [
  {
    icon: FileText,
    title: "Ingest",
    body: "GrokRxiv fetches the paper and prepares the text, equations, references, and figures for review.",
  },
  {
    icon: Workflow,
    title: "Review",
    body: "The paper is evaluated for summary, correctness, novelty, reproducibility, citations, and overall recommendation.",
  },
  {
    icon: GitPullRequest,
    title: "Publish",
    body: "Reviews open on the site as In Review. A moderator approves, rejects, or requests changes before publication.",
  },
];

export function PipelineDiagram() {
  return (
    <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
      {STEPS.map((step, i) => {
        const Icon = step.icon;
        return (
          <div
            key={step.title}
            className="relative rounded-lg border border-[color:var(--color-border)] bg-[color:var(--color-card)] p-6"
          >
            <div className="mb-3 flex items-center gap-3">
              <span className="flex h-9 w-9 items-center justify-center rounded-md bg-[color:var(--color-secondary)] text-[color:var(--color-secondary-foreground)]">
                <Icon className="h-5 w-5" />
              </span>
              <span className="text-xs font-mono uppercase tracking-wider text-[color:var(--color-muted-foreground)]">
                Stage {i + 1}
              </span>
            </div>
            <h3 className="mb-1 text-lg font-semibold">{step.title}</h3>
            <p className="text-sm text-[color:var(--color-muted-foreground)]">
              {step.body}
            </p>
            {i < STEPS.length - 1 ? (
              <ArrowRight className="absolute -right-3 top-1/2 hidden h-6 w-6 -translate-y-1/2 text-[color:var(--color-muted-foreground)] md:block" />
            ) : null}
          </div>
        );
      })}
    </div>
  );
}
