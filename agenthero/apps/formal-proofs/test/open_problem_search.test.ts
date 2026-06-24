import assert from "node:assert/strict";
import test from "node:test";

import {
  normalizeCandidateRecords,
  parseOpenProblemSearchArgs,
  resolveDefaultInputPath
} from "../src/open_problem_search.js";

test("open problem search defaults to app-owned copied pipeline and queue files", () => {
  const parsed = parseOpenProblemSearchArgs([
    "--target",
    "e677-fin-e255",
    "--max-rounds",
    "1",
    "--proposers",
    "fixture"
  ]);

  assert.equal(parsed.pipeline, resolveDefaultInputPath("RESEARCH_PIPELINE.md"));
  assert.equal(parsed.queue, resolveDefaultInputPath("QUEUE.md"));
  assert.equal(parsed.target, "e677-fin-e255");
  assert.equal(parsed.maxRounds, 1);
  assert.deepEqual(parsed.proposers, ["fixture"]);
});

test("malformed candidate records are rejected before leaderboard eligibility", () => {
  const { accepted, rejected } = normalizeCandidateRecords([
    JSON.stringify({
      candidate_id: "fixture-1",
      lane: "E677 =>fin E255",
      locked_open_problem:
        "finite E677=>E255, including all finite orders or a verified countermodel at the frontier",
      claim_type: "countermodel",
      object: { operation_table: [[0]] },
      parameters: { order: 1 },
      claimed_improvement: "fixture only",
      verification_target: "finite_magma_countermodel",
      expected_checker: "haskell",
      proposer: "fixture",
      notes: "syntactic fixture"
    }),
    "{not json}",
    JSON.stringify({
      candidate_id: "missing-fields",
      lane: "E677 =>fin E255"
    })
  ]);

  assert.equal(accepted.length, 1);
  assert.equal(rejected.length, 2);
  assert.equal(accepted[0].verified, false);
});
