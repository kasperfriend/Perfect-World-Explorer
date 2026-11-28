#!/usr/bin/env bash
# Run a CI command and, if it fails, republish its output as annotations.
#
# Why this exists: annotations are the only part of a failing job's output that
# can be read back through the API, and an agent has no other channel. A step
# that just runs a command and dies leaves nothing but "conclusion: failure",
# which has already cost several CI cycles on this branch.
#
# The wrapper never hides the exit status: it re-raises it, so the step still
# fails. It also always echoes the log, for humans reading CI.
#
# usage: tools/ci-run-and-annotate.sh <harness-mode> [--tail N]
set -uo pipefail

mode=${1:?harness mode required}
tail_n=${2:-60}
log=$(mktemp)

set +e
bash tools/offline_harness.sh "$mode" >"$log" 2>&1
status=$?
set -e

cat "$log"

if [ "$status" -ne 0 ]; then
    echo "::error title=harness $mode::exited $status"
    python3 tools/ci-annotate-failures.py "$log" || true
    # The tail is worth having even when a pattern matched, because a test
    # failure's summary line ("N passed; M failed") lives there.
    tail -"$tail_n" "$log" | while IFS= read -r line; do
        line=$(printf '%s' "$line" | cut -c1-380)
        line=${line//%/%25}
        line=${line//$'\r'/%0D}
        echo "::error title=harness tail::$line"
    done
fi

rm -f "$log"
exit "$status"
