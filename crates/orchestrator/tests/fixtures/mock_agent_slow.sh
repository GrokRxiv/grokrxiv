#!/usr/bin/env bash
# Mock agent that sleeps past the test's timeout so the supervisor SIGTERMs it.
set -euo pipefail
cat >/dev/null
sleep 10
echo "{}"
