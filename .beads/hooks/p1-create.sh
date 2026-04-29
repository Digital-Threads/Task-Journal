#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

declare -A TITLES=(
  [01]="Install Rust toolchain in WSL"
  [02]="Cargo workspace skeleton 3 crates"
  [03]="EventType enum with 12 variants"
  [04]="Author Source EventStatus EvidenceStrength enums"
  [05]="Refs and Event structs with serde"
  [06]="Event new constructor ULID RFC3339"
  [07]="JsonlWriter append plus fsync"
  [08]="paths data_dir for current OS"
  [09]="project_hash from_path"
  [10]="SQLite open plus initial migration"
  [11]="Tasks repo upsert_task_from_event"
  [12]="index_event writes events_index plus search_fts"
  [13]="rebuild_state from JSONL"
  [14]="Integration test full core round-trip"
  [15]="Add rmcp to tj-mcp workspace dep"
  [16]="rmcp server skeleton with empty tool router"
  [17]="Stub tool task_pack"
  [18]="Stub tools task_search task_create event_add task_close"
  [19]="CLI scaffolding with clap"
  [20]="CLI create writes open event to JSONL"
  [21]="CLI events list reads JSONL"
  [22]="CLI rebuild-state replays JSONL into SQLite"
)

MAP=/home/shahinyanm/www/claude-memory/.docs/plans/2026-04-29-p1-task-map.txt
mkdir -p "$(dirname "$MAP")"
: > "$MAP"
echo "# P1 task map: plan-task# -> bd-id" >> "$MAP"
echo "# epic: claude-memory-d36" >> "$MAP"
echo "" >> "$MAP"

for i in 01 02 03 04 05 06 07 08 09 10 11 12 13 14 15 16 17 18 19 20 21 22; do
  TITLE="P1.$i: ${TITLES[$i]}"
  DESC="Phase 1 skeleton. See .docs/plans/2026-04-29-task-journal-v1-p1-skeleton.md Task $i for full step-by-step plan."
  ACC="RED test from Step 1 passes after Step 3 impl. Step 5 commit landed. bd close with reason."

  RESULT=$(bd create --title "$TITLE" --type=task --priority=1 --description "$DESC" --acceptance "$ACC" --json 2>&1 || true)
  ID=$(echo "$RESULT" | grep -oE "claude-memory-[a-z0-9]+" | head -1)
  if [ -z "$ID" ]; then
    echo "FAILED to create P1.$i:" >&2
    echo "$RESULT" >&2
    exit 1
  fi
  echo "$i  $ID" >> "$MAP"
  echo "P1.$i -> $ID"
done

echo ""
echo "Map saved to $MAP"
cat "$MAP"
