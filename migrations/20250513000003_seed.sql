-- GrokRxiv — local-dev seed data.
-- Inserts one sample paper and one matching published review so the Next.js
-- app has something to render against a clean Supabase instance.
-- Safe to re-run: keyed on the deterministic arxiv_id.

insert into papers (id, arxiv_id, title, authors, abstract, field, submitted_date, ingested_at)
values (
  '11111111-1111-1111-1111-111111111111',
  '2401.12345',
  'Modular Verifier-Gated Pipelines for Multi-Agent Peer Review',
  '[{"name":"Ada Lovelace","affiliation":"GrokRxiv Labs"},{"name":"Alan Turing","affiliation":"GrokRxiv Labs"}]'::jsonb,
  'We introduce a verifier-gated DAG of specialist LLM agents that produce typed, human-readable peer reviews of arXiv papers. The pipeline enforces JSON-schema validity, citation existence, and tone budgets at every hop.',
  'cs.AI',
  date '2026-05-01',
  now()
)
on conflict (arxiv_id) do nothing;

insert into reviews (
  id, paper_id, status,
  github_pr_url, github_review_url,
  html_path, pdf_path, zip_path,
  models_used, meta_review,
  created_at, published_at
)
values (
  '22222222-2222-2222-2222-222222222222',
  '11111111-1111-1111-1111-111111111111',
  'published',
  'https://github.com/GrokRxiv/reviews/pull/1',
  'https://grokrxiv.org/reviews/2401.12345',
  'renders/2401.12345/review.html',
  'renders/2401.12345/review.pdf',
  'bundles/2401.12345/bundle.zip',
  '{
     "summary":"claude-opus-4-7",
     "technical_correctness":"claude-opus-4-7",
     "novelty":"gemini-2.5-pro",
     "reproducibility":"o4-mini",
     "citation":"gemini-2.5-flash",
     "meta_reviewer":"claude-opus-4-7"
   }'::jsonb,
  '{
     "summary":"A practical architecture for verifier-gated multi-agent review of arXiv submissions.",
     "strengths":["Typed artifacts with JSON-schema validation","Human moderation PR before publication"],
     "weaknesses":["Limited empirical evaluation","Tone classifier is rule-based at MVP"],
     "questions":["How does the pipeline degrade when a single specialist agent times out?"],
     "recommendation":"minor_revision",
     "confidence":0.78
   }'::jsonb,
  now() - interval '1 day',
  now() - interval '12 hours'
)
on conflict (id) do nothing;

insert into review_agents (review_id, role, model, output, verifier_status, tokens_in, tokens_out, latency_ms)
values
  ('22222222-2222-2222-2222-222222222222','summary','claude-opus-4-7',
   '{
      "plain_language_summary":"The authors propose a way to peer-review arXiv papers with a small team of cooperating language models, each checking a different aspect.",
      "key_contributions":[
        "A typed DAG of specialist reviewer agents",
        "A verifier ladder that gates every artifact",
        "A human moderation surface before public publication"
      ],
      "audience":"AI systems researchers and academic publishers interested in automated peer review.",
      "tldr":"A verifier-gated multi-agent pipeline for AI-generated peer reviews of arXiv papers."
    }'::jsonb,
   'pass', 2400, 380, 9100),
  ('22222222-2222-2222-2222-222222222222','meta_reviewer','claude-opus-4-7',
   '{"summary":"Solid architecture, weak evaluation.","recommendation":"minor_revision","confidence":0.78,"strengths":[],"weaknesses":[],"questions":[]}'::jsonb,
   'pass', 5400, 720, 14300)
on conflict do nothing;
