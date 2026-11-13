#!/usr/bin/env bash
# Usage: logged.sh <title> <command...>
# Runs a command, and on failure publishes the end of its output as an error
# annotation, which is readable without signing in to GitHub (unlike the logs).
set -uo pipefail
title="$1"; shift
log="${RUNNER_TEMP:-/tmp}/logged-$$.log"
"$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
if [ "$status" -ne 0 ]; then
    tail -n 60 "$log" | tr -d '\r' | sed 's/%/%25/g' | awk 'BEGIN{ORS="%0A"} {print}' > "$log.tail"
    echo "::error title=$title failed::$(cat "$log.tail")"
fi
exit "$status"
