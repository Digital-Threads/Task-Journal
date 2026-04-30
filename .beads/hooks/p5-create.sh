#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

declare -A TITLES=(
  [01]="plugin skeleton with .claude-plugin/plugin.json manifest"
  [02]="plugin .mcp.json with wsl-wrapped task-journal-mcp"
  [03]="slash command /task-journal:create"
  [04]="slash command /task-journal:event"
  [05]="slash command /task-journal:pack"
  [06]="slash command /task-journal:search"
  [07]="slash command /task-journal:close"
  [08]="slash command /task-journal:stats"
  [09]="skill task-journal SKILL.md when to use tools"
  [10]="declarative hooks in plugin.json for auto-capture"
  [11]="README plugin installation section"
  [12]="P5 verification install plugin locally"
)

MAP=/home/shahinyanm/www/claude-memory/.docs/plans/2026-04-30-p5-task-map.txt
: > "$MAP"
echo "# P5 task map: plan-task# -> bd-id" >> "$MAP"
echo "# epic: claude-memory-d36" >> "$MAP"
echo "" >> "$MAP"

for i in 01 02 03 04 05 06 07 08 09 10 11 12; do
  TITLE="P5.$i: ${TITLES[$i]}"
  DESC="Phase 5 Claude Code plugin wrapper. See p5 plan."
  ACC="Files created. Plugin loadable. Tests pass if applicable."
  RESULT=$(bd create --title "$TITLE" --type=task --priority=1 --description "$DESC" --acceptance "$ACC" --json 2>&1 || true)
  ID=$(echo "$RESULT" | grep -oE "claude-memory-[a-z0-9]+" | head -1)
  if [ -z "$ID" ]; then
    echo "FAILED P5.$i: $RESULT" >&2
    exit 1
  fi
  echo "$i  $ID" >> "$MAP"
  echo "P5.$i -> $ID"
done

mapfile -t LINES < <(grep -E "^[0-9]+ " "$MAP")
PREV_ID=""
for line in "${LINES[@]}"; do
  i=$(echo "$line" | awk '{print $1}')
  id=$(echo "$line" | awk '{print $2}')
  bd link "$id" claude-memory-d36 --type=parent-child >/dev/null 2>&1 && true
  if [ -n "$PREV_ID" ]; then
    bd link "$id" "$PREV_ID" --type=blocks >/dev/null 2>&1 && true
  fi
  PREV_ID="$id"
done

echo "Map saved to $MAP"
