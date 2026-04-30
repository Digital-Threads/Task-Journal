#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

declare -A TITLES=(
  [01]="Add ureq HTTP client and mockito test dep"
  [02]="classifier module skeleton with types"
  [03]="MockClassifier canned-response driver"
  [04]="Classifier prompt builder"
  [05]="Anthropic HTTP client AnthropicClassifier"
  [06]="decide_status confidence gating helper"
  [07]="pack render question-mark marker for suggested events"
  [08]="CLI ingest-hook with mock writes classified event"
  [09]="CLI ingest-hook real Anthropic classifier path"
  [10]="Pending queue replay on next ingest"
  [11]="CLI event-correct subcommand"
  [12]="CLI install-hooks writes to settings.json"
  [13]="install-hooks idempotency plus uninstall"
  [14]="pack Corrections marker via correction events"
  [15]="classifier prompt size bound under 64KB"
  [16]="E2E hook simulation classifies and packs"
  [17]="Manual smoke with real Anthropic API"
  [18]="P3 verification gate"
)

MAP=/home/shahinyanm/www/claude-memory/.docs/plans/2026-04-30-p3-task-map.txt
mkdir -p "$(dirname "$MAP")"
: > "$MAP"
echo "# P3 task map: plan-task# -> bd-id" >> "$MAP"
echo "# epic: claude-memory-d36" >> "$MAP"
echo "" >> "$MAP"

for i in 01 02 03 04 05 06 07 08 09 10 11 12 13 14 15 16 17 18; do
  TITLE="P3.$i: ${TITLES[$i]}"
  DESC="Phase 3 hooks + classifier. See .docs/plans/2026-04-30-task-journal-v1-p3-hooks-classifier.md Task $i for full step-by-step."
  ACC="RED test from Step 1 passes after Step 3 impl. Step commit landed. bd close with reason."

  RESULT=$(bd create --title "$TITLE" --type=task --priority=1 --description "$DESC" --acceptance "$ACC" --json 2>&1 || true)
  ID=$(echo "$RESULT" | grep -oE "claude-memory-[a-z0-9]+" | head -1)
  if [ -z "$ID" ]; then
    echo "FAILED to create P3.$i:" >&2
    echo "$RESULT" >&2
    exit 1
  fi
  echo "$i  $ID" >> "$MAP"
  echo "P3.$i -> $ID"
done

echo ""
echo "Created $(grep -cE '^[0-9]+ ' "$MAP") tasks"
