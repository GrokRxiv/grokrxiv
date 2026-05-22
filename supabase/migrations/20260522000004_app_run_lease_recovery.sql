-- Add durable app-run lease recovery fields and recover already-expired runs.

alter table app_runs add column if not exists attempt int not null default 0;
alter table app_runs add column if not exists recovered_at timestamptz;
alter table app_runs add column if not exists last_lease_expired_at timestamptz;

with expired as (
  update worker_leases wl
     set state = 'expired',
         updated_at = now()
    from app_runs ar
   where wl.app_run_id = ar.id
     and wl.state = 'leased'
     and wl.leased_until < now()
     and ar.state = 'running'
     and ar.attempt < 2
   returning wl.app_run_id
)
update app_runs ar
   set state = 'queued',
       recovered_at = now(),
       last_lease_expired_at = now(),
       updated_at = now(),
       error_code = 'lease_expired',
       error_message = 'worker lease expired; run requeued',
       error_retryable = true
  from expired
 where ar.id = expired.app_run_id;

with expired as (
  update worker_leases wl
     set state = 'expired',
         updated_at = now()
    from app_runs ar
   where wl.app_run_id = ar.id
     and wl.state = 'leased'
     and wl.leased_until < now()
     and ar.state = 'running'
     and ar.attempt >= 2
   returning wl.app_run_id
)
update app_runs ar
   set state = 'system_failed',
       finished_at = coalesce(finished_at, now()),
       last_lease_expired_at = now(),
       updated_at = now(),
       error_code = 'lease_expired',
       error_message = 'worker lease expired after retry',
       error_retryable = true
  from expired
 where ar.id = expired.app_run_id;
