-- Account review workflow, quota claims, and super-admin controls.

alter table user_roles
  drop constraint if exists user_roles_role_check;

alter table user_roles
  add constraint user_roles_role_check
  check (role in ('user','moderator','admin','super_admin'));

create or replace function public.grokrxiv_is_super_admin()
returns boolean
language sql
stable
security definer
set search_path = public
as $$
  select public.grokrxiv_current_user_role() = 'super_admin'
$$;

create or replace function public.grokrxiv_is_admin()
returns boolean
language sql
stable
security definer
set search_path = public
as $$
  select public.grokrxiv_current_user_role() in ('admin','super_admin')
$$;

create or replace function public.grokrxiv_is_moderator_or_admin()
returns boolean
language sql
stable
security definer
set search_path = public
as $$
  select public.grokrxiv_current_user_role() in ('moderator','admin','super_admin')
$$;

grant execute on function public.grokrxiv_is_super_admin() to anon, authenticated;
grant execute on function public.grokrxiv_is_admin() to anon, authenticated;
grant execute on function public.grokrxiv_is_moderator_or_admin() to anon, authenticated;

alter table submissions
  add column if not exists paper_id uuid references papers(id) on delete set null;

create index if not exists submissions_paper_idx on submissions(paper_id);
create index if not exists submissions_user_quota_idx
  on submissions(user_id, visibility, compute_profile, created_at desc)
  where quota_charged = true;

drop policy if exists papers_submitter_read on papers;
create policy papers_submitter_read on papers
  for select
  to authenticated
  using (
    exists (
      select 1 from reviews r
      where r.paper_id = papers.id
        and r.submitted_by = auth.uid()
    )
  );

drop policy if exists papers_moderator_read on papers;
create policy papers_moderator_read on papers
  for select
  to authenticated
  using (public.grokrxiv_is_moderator_or_admin());

create or replace function public.grokrxiv_claim_review_submission(
  target_user_id uuid,
  target_source text,
  target_source_type text default 'arxiv',
  target_visibility text default 'public',
  requested_compute_profile text default null,
  target_public_consent boolean default false
)
returns table (
  submission_id uuid,
  plan_id text,
  compute_profile text,
  visibility text,
  public_used int,
  public_limit int,
  private_used int,
  private_limit int,
  remaining_public int,
  remaining_private int
)
language plpgsql
security definer
set search_path = public
as $$
declare
  v_role text;
  v_plan_id text;
  v_billing_status text;
  v_allow_private boolean;
  v_public_monthly int;
  v_private_monthly int;
  v_lifetime_public int;
  v_override int;
  v_compute text;
  v_public_used int;
  v_private_used int;
  v_public_limit int;
  v_private_limit int;
  v_period_start timestamptz;
  v_period_end timestamptz;
  v_submission_id uuid;
begin
  if target_user_id is null then
    raise exception 'target_user_id_required' using errcode = '22023';
  end if;
  if nullif(btrim(target_source), '') is null then
    raise exception 'source_required' using errcode = '22023';
  end if;
  if target_source_type not in ('arxiv','pdf','tex','mixed') then
    raise exception 'bad_source_type' using errcode = '22023';
  end if;
  if target_visibility not in ('public','private') then
    raise exception 'bad_visibility' using errcode = '22023';
  end if;
  if requested_compute_profile is not null and requested_compute_profile not in (
    'public_free','paid_standard','paid_private','premium_api'
  ) then
    raise exception 'bad_compute_profile' using errcode = '22023';
  end if;

  perform public.grokrxiv_ensure_user_account(target_user_id, null);
  perform pg_advisory_xact_lock(hashtextextended(target_user_id::text, 540701));

  select ur.role
  into v_role
  from user_roles ur
  where ur.user_id = target_user_id;

  select p.review_limit_override
  into v_override
  from profiles p
  where p.user_id = target_user_id;

  select ub.plan_id, ub.status, ub.period_start, ub.period_end
  into v_plan_id, v_billing_status, v_period_start, v_period_end
  from user_billing ub
  where ub.user_id = target_user_id;

  if v_role in ('admin','super_admin') then
    v_plan_id := 'admin';
    v_billing_status := 'active';
  elsif v_billing_status not in ('active','trialing') then
    v_plan_id := 'free';
  end if;
  v_plan_id := coalesce(v_plan_id, 'free');

  select
    bp.public_reviews_per_month,
    bp.private_reviews_per_month,
    bp.lifetime_public_reviews,
    bp.allow_private
  into
    v_public_monthly,
    v_private_monthly,
    v_lifetime_public,
    v_allow_private
  from billing_plans bp
  where bp.id = v_plan_id;

  if not found then
    raise exception 'billing_plan_not_found' using errcode = 'P0001';
  end if;

  if target_visibility = 'public' and not coalesce(target_public_consent, false) then
    insert into quota_events (
      user_id, event_type, visibility, compute_profile, reason, metadata
    ) values (
      target_user_id, 'blocked', 'public', null, 'public_consent_required',
      jsonb_build_object('source', target_source, 'source_type', target_source_type)
    );
    raise exception 'public_consent_required' using errcode = 'P0001';
  end if;

  if target_visibility = 'private' and not v_allow_private then
    insert into quota_events (
      user_id, event_type, visibility, compute_profile, reason, metadata
    ) values (
      target_user_id, 'blocked', 'private', null, 'private_reviews_require_paid_plan',
      jsonb_build_object('source', target_source, 'source_type', target_source_type)
    );
    raise exception 'private_reviews_require_paid_plan' using errcode = 'P0001';
  end if;

  if requested_compute_profile is not null then
    v_compute := requested_compute_profile;
  elsif target_visibility = 'private' then
    v_compute := 'paid_private';
  elsif v_plan_id = 'free' then
    v_compute := 'public_free';
  else
    v_compute := 'paid_standard';
  end if;

  if target_visibility = 'public' and v_compute not in ('public_free','paid_standard','premium_api') then
    raise exception 'bad_compute_profile_for_public_review' using errcode = '22023';
  end if;
  if target_visibility = 'private' and v_compute not in ('paid_private','premium_api') then
    raise exception 'bad_compute_profile_for_private_review' using errcode = '22023';
  end if;

  if v_plan_id = 'free' then
    select count(*)::int
    into v_public_used
    from submissions s
    where s.user_id = target_user_id
      and s.visibility = 'public'
      and s.compute_profile <> 'sample_preview'
      and s.quota_charged = true
      and s.state not in ('failed','system_failed');
    v_public_limit := coalesce(v_override, v_lifetime_public, 3);
  else
    v_period_start := coalesce(v_period_start, date_trunc('month', now()));
    v_period_end := coalesce(v_period_end, v_period_start + interval '1 month');
    select count(*)::int
    into v_public_used
    from submissions s
    where s.user_id = target_user_id
      and s.visibility = 'public'
      and s.compute_profile <> 'sample_preview'
      and s.quota_charged = true
      and s.state not in ('failed','system_failed')
      and s.created_at >= v_period_start
      and s.created_at < v_period_end;
    v_public_limit := coalesce(v_override, v_public_monthly);
  end if;

  v_period_start := coalesce(v_period_start, date_trunc('month', now()));
  v_period_end := coalesce(v_period_end, v_period_start + interval '1 month');
  select count(*)::int
  into v_private_used
  from submissions s
  where s.user_id = target_user_id
    and s.visibility = 'private'
    and s.compute_profile <> 'sample_preview'
    and s.quota_charged = true
    and s.state not in ('failed','system_failed')
    and s.created_at >= v_period_start
    and s.created_at < v_period_end;
  v_private_limit := v_private_monthly;

  if target_visibility = 'public' and v_public_used >= v_public_limit then
    insert into quota_events (
      user_id, event_type, visibility, compute_profile, amount, reason, metadata
    ) values (
      target_user_id, 'blocked', 'public', v_compute, 1, 'public_quota_exhausted',
      jsonb_build_object('source', target_source, 'source_type', target_source_type)
    );
    raise exception 'public_quota_exhausted' using errcode = 'P0001';
  end if;

  if target_visibility = 'private' and v_private_used >= v_private_limit then
    insert into quota_events (
      user_id, event_type, visibility, compute_profile, amount, reason, metadata
    ) values (
      target_user_id, 'blocked', 'private', v_compute, 1, 'private_quota_exhausted',
      jsonb_build_object('source', target_source, 'source_type', target_source_type)
    );
    raise exception 'private_quota_exhausted' using errcode = 'P0001';
  end if;

  insert into submissions (
    user_id, source, source_type, visibility, compute_profile, state,
    quota_charged
  ) values (
    target_user_id, btrim(target_source), target_source_type, target_visibility,
    v_compute, 'queued', true
  )
  returning id into v_submission_id;

  insert into quota_events (
    user_id, submission_id, event_type, visibility, compute_profile, amount,
    reason, metadata
  ) values (
    target_user_id, v_submission_id, 'accepted', target_visibility, v_compute, 1,
    'review_submission_accepted',
    jsonb_build_object('plan_id', v_plan_id, 'source_type', target_source_type)
  );

  insert into quota_events (
    user_id, submission_id, event_type, visibility, compute_profile, amount,
    reason, metadata
  ) values (
    target_user_id, v_submission_id, 'charged', target_visibility, v_compute, 1,
    'review_submission_quota_charged',
    jsonb_build_object('plan_id', v_plan_id, 'source_type', target_source_type)
  );

  if target_visibility = 'public' then
    v_public_used := v_public_used + 1;
  else
    v_private_used := v_private_used + 1;
  end if;

  return query select
    v_submission_id,
    v_plan_id,
    v_compute,
    target_visibility,
    v_public_used,
    v_public_limit,
    v_private_used,
    v_private_limit,
    greatest(v_public_limit - v_public_used, 0),
    greatest(v_private_limit - v_private_used, 0);
end;
$$;

grant execute on function public.grokrxiv_claim_review_submission(
  uuid, text, text, text, text, boolean
) to service_role;

create or replace function public.grokrxiv_mark_submission_running(
  target_submission_id uuid,
  target_paper_id uuid default null
)
returns void
language plpgsql
security definer
set search_path = public
as $$
begin
  update submissions
  set
    state = 'running',
    paper_id = coalesce(target_paper_id, paper_id),
    error = null,
    updated_at = now()
  where id = target_submission_id;
end;
$$;

grant execute on function public.grokrxiv_mark_submission_running(uuid, uuid)
  to service_role;

create or replace function public.grokrxiv_mark_submission_review_ready(
  target_submission_id uuid,
  target_review_id uuid,
  target_paper_id uuid,
  target_visibility text
)
returns void
language plpgsql
security definer
set search_path = public
as $$
begin
  update submissions
  set
    state = case
      when target_visibility = 'private' then 'private_ready'
      else 'awaiting_moderation'
    end,
    review_id = target_review_id,
    paper_id = coalesce(target_paper_id, paper_id),
    error = null,
    updated_at = now()
  where id = target_submission_id;
end;
$$;

grant execute on function public.grokrxiv_mark_submission_review_ready(
  uuid, uuid, uuid, text
) to service_role;

create or replace function public.grokrxiv_mark_submission_failed(
  target_submission_id uuid,
  target_error text,
  refund_quota boolean default true
)
returns void
language plpgsql
security definer
set search_path = public
as $$
declare
  v_user_id uuid;
  v_visibility text;
  v_compute_profile text;
  v_was_charged boolean;
begin
  select user_id, visibility, compute_profile, quota_charged
  into v_user_id, v_visibility, v_compute_profile, v_was_charged
  from submissions
  where id = target_submission_id;

  update submissions
  set
    state = 'system_failed',
    quota_charged = case when refund_quota then false else quota_charged end,
    error = left(coalesce(target_error, 'review dispatch failed'), 2000),
    updated_at = now()
  where id = target_submission_id;

  if refund_quota and coalesce(v_was_charged, false) then
    insert into quota_events (
      user_id, submission_id, event_type, visibility, compute_profile, amount,
      reason, metadata
    ) values (
      v_user_id, target_submission_id, 'refunded', v_visibility,
      v_compute_profile, 1, 'system_failed',
      jsonb_build_object('error', left(coalesce(target_error, ''), 500))
    );
  end if;
end;
$$;

grant execute on function public.grokrxiv_mark_submission_failed(uuid, text, boolean)
  to service_role;
