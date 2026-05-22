import { randomUUID } from "node:crypto";
import { existsSync, readFileSync } from "node:fs";
import { resolve } from "node:path";
import { createClient } from "@supabase/supabase-js";

const rootEnv = resolve(process.cwd(), "../../.env");
loadEnv(rootEnv);
loadIncludedEnv(resolve(process.cwd(), "../.."));
loadEnv(resolve(process.cwd(), ".env.local"));

const email = (process.env.GROKRXIV_SUPER_ADMIN_EMAIL ?? "").trim().toLowerCase();
const internalSupabaseUrl = process.env.SUPABASE_INTERNAL_URL ?? "";
const browserSupabaseUrl = process.env.NEXT_PUBLIC_SUPABASE_URL ?? "";
const supabaseUrl =
  internalSupabaseUrl.includes("host.docker.internal") && browserSupabaseUrl
    ? browserSupabaseUrl
    : internalSupabaseUrl || browserSupabaseUrl;
const serviceRoleKey = process.env.SUPABASE_SERVICE_ROLE_KEY ?? "";

if (!email) {
  console.log("GROKRXIV_SUPER_ADMIN_EMAIL is unset; no super admin seeded.");
  process.exit(0);
}
if (!supabaseUrl || !serviceRoleKey) {
  throw new Error("Supabase URL and SUPABASE_SERVICE_ROLE_KEY are required.");
}

const supabase = createClient(supabaseUrl, serviceRoleKey, {
  auth: { persistSession: false, autoRefreshToken: false },
});

const user = await ensureAuthUser(email);
await supabase.rpc("grokrxiv_ensure_user_account", {
  target_user_id: user.id,
  target_email: email,
});

await must(
  supabase.from("user_roles").upsert(
    {
      user_id: user.id,
      role: "super_admin",
      updated_at: new Date().toISOString(),
    },
    { onConflict: "user_id" },
  ),
);
await must(
  supabase
    .from("profiles")
    .update({
      display_name: email,
      billing_tier: "admin",
      updated_at: new Date().toISOString(),
    })
    .eq("user_id", user.id),
);
await must(
  supabase.from("user_billing").upsert(
    {
      user_id: user.id,
      plan_id: "admin",
      status: "active",
      provider: "manual",
      updated_at: new Date().toISOString(),
    },
    { onConflict: "user_id" },
  ),
);

console.log(`Seeded super admin ${email} (${user.id}).`);

async function ensureAuthUser(targetEmail) {
  const existing = await findUserByEmail(targetEmail);
  if (existing) return existing;

  const { data, error } = await supabase.auth.admin.createUser({
    email: targetEmail,
    email_confirm: true,
    password: `GrokRxiv-${randomUUID()}-local`,
  });
  if (error) {
    const retry = await findUserByEmail(targetEmail);
    if (retry) return retry;
    throw new Error(error.message);
  }
  if (!data.user) throw new Error("Supabase did not return a created user.");
  return data.user;
}

async function findUserByEmail(targetEmail) {
  let page = 1;
  for (;;) {
    const { data, error } = await supabase.auth.admin.listUsers({
      page,
      perPage: 200,
    });
    if (error) throw new Error(error.message);
    const found = data.users.find(
      (candidate) => candidate.email?.toLowerCase() === targetEmail,
    );
    if (found) return found;
    if (data.users.length < 200) return null;
    page += 1;
  }
}

async function must(resultPromise) {
  const { error } = await resultPromise;
  if (error) throw new Error(error.message);
}

function loadEnv(path) {
  if (!existsSync(path)) return;
  for (const line of readFileSync(path, "utf8").split(/\r?\n/)) {
    const trimmed = line.trim();
    if (!trimmed || trimmed.startsWith("#")) continue;
    const match = trimmed.match(/^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/);
    if (!match) continue;
    const [, key, rawValue] = match;
    if (process.env[key] !== undefined) continue;
    process.env[key] = rawValue.replace(/^['"]|['"]$/g, "");
  }
}

function loadIncludedEnv(rootDir) {
  const envFiles = process.env.AGENTHERO_ENV_FILES ?? "";
  for (const entry of envFiles.split(",")) {
    const trimmed = entry.trim();
    if (!trimmed) continue;
    const path = trimmed.startsWith("/") ? trimmed : resolve(rootDir, trimmed);
    if (!existsSync(path)) {
      throw new Error(`AGENTHERO_ENV_FILES references missing file ${path}`);
    }
    loadEnv(path);
  }
}
