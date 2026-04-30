#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

declare -A TITLES=(
  [01]="pack module skeleton plus TaskPack types"
  [02]="pack assemble minimum header only"
  [03]="pack lifecycle history section"
  [04]="project decision events into decisions table"
  [05]="supersede event marks decision superseded"
  [06]="project evidence events into evidence table"
  [07]="pack render Active decisions section"
  [08]="pack render Rejected section"
  [09]="pack render Evidence section"
  [10]="pack render Recent events section"
  [11]="compact mode omits optional sections"
  [12]="pack_cache read-through"
  [13]="pack_cache invalidation on new event"
  [14]="CLI pack subcommand"
  [15]="CLI event subcommand"
  [16]="CLI close subcommand"
  [17]="CLI search subcommand FTS5"
  [18]="MCP task_pack real impl"
  [19]="MCP create event_add close search real impls"
  [20]="Golden fixture A compact 5-event pack"
  [21]="Golden fixture B full 12-event with supersede correction"
  [22]="E2E CLI test create event close pack search"
  [23]="Verification gate plus plan finish"
)

MAP=/home/shahinyanm/www/claude-memory/.docs/plans/2026-04-30-p2-task-map.txt
mkdir -p "$(dirname "$MAP")"
: > "$MAP"
echo "# P2 task map: plan-task# -> bd-id" >> "$MAP"
echo "# epic: claude-memory-d36" >> "$MAP"
echo "" >> "$MAP"

for i in 01 02 03 04 05 06 07 08 09 10 11 12 13 14 15 16 17 18 19 20 21 22 23; do
  TITLE="P2.$i: ${TITLES[$i]}"
  DESC="Phase 2 task_pack core. See .docs/plans/2026-04-30-task-journal-v1-p2-task-pack-core.md Task $i for full step-by-step plan."
  ACC="RED test from Step 1 passes after Step 3 impl. Step commit landed. bd close with reason."

  RESULT=$(bd create --title "$TITLE" --type=task --priority=1 --description "$DESC" --acceptance "$ACC" --json 2>&1 || true)
  ID=$(echo "$RESULT" | grep -oE "claude-memory-[a-z0-9]+" | head -1)
  if [ -z "$ID" ]; then
    echo "FAILED to create P2.$i:" >&2
    echo "$RESULT" >&2
    exit 1
  fi
  echo "$i  $ID" >> "$MAP"
  echo "P2.$i -> $ID"
done

echo ""
echo "Created $(grep -cE '^[0-9]+ ' "$MAP") tasks"
echo "Map saved to $MAP"
