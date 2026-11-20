#!/usr/bin/env bash
# Run a CI command and, if it fails, republish its output as annotations.
#
# Why this exists: annotations are the only part of a failing job's output that
# can be read back through the API. A step that just runs a command and dies
# leaves an agent with "conclusion: failure" and nothing else. This wrapper
# never hides the exit status -- it re-raises it, so the step still fails.
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

# Always show the output in the step log as well: this is for humans reading CI.
cat "$log"

if [ "$status" -ne 0 ]; then
    echo "::error title=harness $mode::exited $status"
    # The interesting part is at the end for a test failure and at the start for
    # a compile error, so publish both ends.
    {
        head -25 "$log"
        echo '... ---- (tail) ---- ...'
        tail -"$tail_n" "$log"
    } | sed 's/%/%25/g; s/\r/%0D/g' | python3 -c '
import sys
n = 0
buf = []
size = 0
for raw in sys.stdin:
    line = raw.rstrip("\n")[:360]
    if line.startswith("::") or line.startswith("echo "):
        continue
    buf.append(line)
    size += len(line)
    if size >= 380:
        print("::error title=harness output part {}::{}".format(n, " | ".join(buf)))
        n += 1
        buf, size = [], 0
if buf:
    print("::error title=harness output part {}::{}".format(n, " | ".join(buf)))
'
fi

rm -f "$log"
exit "$status"
