import { forwardToOrchestrator, requireServiceToken } from "../_lib";

// `/api/v1/doctor` proxies the orchestrator's preflight report so operators
// can hit the same surface from the web tier as from the CLI.
export async function GET(req: Request) {
  const unauth = requireServiceToken(req);
  if (unauth) return unauth;
  return forwardToOrchestrator("/internal/v1/doctor", { method: "GET" });
}
