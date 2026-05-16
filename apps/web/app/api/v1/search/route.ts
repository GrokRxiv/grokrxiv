import { notImplemented, requireServiceToken } from "../_lib";

// `/api/v1/search` is reserved for the cross-corpus search endpoint
// (Track I follow-up). The CLI ships with `grokrxiv list` / `grokrxiv show`
// in the meantime.
export async function GET(req: Request) {
  const unauth = requireServiceToken(req);
  if (unauth) return unauth;
  return notImplemented(
    "Search endpoint is a Track I follow-up. Use /api/v1/reviews for listing.",
  );
}
