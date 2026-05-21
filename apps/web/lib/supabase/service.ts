import { createClient } from "@supabase/supabase-js";
import {
  SUPABASE_SERVICE_ROLE_KEY,
  SUPABASE_URL,
  isSupabaseConfigured,
} from "@/lib/env";

export function isSupabaseServiceConfigured(): boolean {
  return isSupabaseConfigured() && SUPABASE_SERVICE_ROLE_KEY.length > 0;
}

export function createSupabaseServiceClient() {
  if (!isSupabaseServiceConfigured()) {
    throw new Error("Supabase service role is not configured.");
  }
  return createClient(SUPABASE_URL, SUPABASE_SERVICE_ROLE_KEY, {
    auth: { persistSession: false, autoRefreshToken: false },
  });
}
