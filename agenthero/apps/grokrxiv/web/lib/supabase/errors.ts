type SupabaseLikeError = {
  code?: string;
  message?: string;
  details?: string;
  hint?: string;
};

export function supabaseErrorCode(error: SupabaseLikeError): string {
  if (isMissingVisibilityColumn(error)) return "schema_not_applied";
  return "supabase_query_failed";
}

export function supabaseErrorMessage(error: SupabaseLikeError): string {
  if (isMissingVisibilityColumn(error)) {
    return "Review data is temporarily unavailable while the site is being updated.";
  }
  return error.message ?? "Review data is temporarily unavailable.";
}

function isMissingVisibilityColumn(error: SupabaseLikeError): boolean {
  const text = [
    error.code,
    error.message,
    error.details,
    error.hint,
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  return text.includes("reviews.visibility") || text.includes("visibility");
}
