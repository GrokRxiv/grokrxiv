import type { AgentOutput } from "@/lib/types";

function agentOutputKey(agent: AgentOutput): string {
  const dagType = agent.dag_type ?? "paper-review";
  const node = agent.node_id ?? agent.role;
  return `${dagType}:${node}`;
}

function createdAtMs(agent: AgentOutput): number {
  if (!agent.created_at) return 0;
  const parsed = Date.parse(agent.created_at);
  return Number.isFinite(parsed) ? parsed : 0;
}

export function latestAgentOutputs(
  agents: AgentOutput[] | null | undefined,
): AgentOutput[] {
  const latest = new Map<string, AgentOutput>();
  for (const agent of [...(agents ?? [])].sort(
    (a, b) => createdAtMs(b) - createdAtMs(a),
  )) {
    const key = agentOutputKey(agent);
    if (!latest.has(key)) {
      latest.set(key, agent);
    }
  }
  return [...latest.values()];
}

export function withLatestAgentOutputs<T extends { agents?: AgentOutput[] | null }>(
  review: T,
): T {
  return {
    ...review,
    agents: latestAgentOutputs(review.agents),
  };
}
