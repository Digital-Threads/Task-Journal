#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

declare -A TITLES=(
  [01]="db list_all_projects helper"
  [02]="CLI search --all-projects flag"
  [03]="Pack token-budget truncation full mode"
  [04]="Pack metadata truncated field"
  [05]="Classifier telemetry append-only writer"
  [06]="CLI stats subcommand"
  [07]="ingest-hook writes telemetry"
  [08]="Install-hooks adds || true to suppress failures"
  [09]="Gate --mock-* flags behind test-helpers feature"
  [10]="README install + usage"
  [11]="INSTALL hook walkthrough"
  [12]="P4 verification gate plus demo"
)

MAP=/home/shahinyanm/www/claude-memory/.docs/plans/2026-04-30-p4-task-map.txt
mkdir -p "$(dirname "$MAP")"
: > "$MAP"
echo "# P4 task map: plan-task# -> bd-id" >> "$MAP"
echo "# epic: claude-memory-d36" >> "$MAP"
echo "" >> "$MAP"

for i in 01 02 03 04 05 06 07 08 09 10 11 12; do
  TITLE="P4.$i: ${TITLES[$i]}"
  DESC="Phase 4 polish + dogfood. See .docs/plans/2026-04-30-task-journal-v1-p4-polish-dogfood.md Task $i."
  ACC="RED test passes after impl. Commit landed. bd close with reason."

  RESULT=$(bd create --title "$TITLE" --type=task --priority=1 --description "$DESC" --acceptance "$ACC" --json 2>&1 || true)
  ID=$(echo "$RESULT" | grep -oE "claude-memory-[a-z0-9]+" | head -1)
  if [ -z "$ID" ]; then
    echo "FAILED to create P4.$i:" >&2
    echo "$RESULT" >&2
    exit 1
  fi
  echo "$i  $ID" >> "$MAP"
  echo "P4.$i -> $ID"
done
echo ""
echo "Created $(grep -cE '^[0-9]+ ' "$MAP") tasks"
