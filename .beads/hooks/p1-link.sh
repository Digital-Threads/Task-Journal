#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

EPIC=claude-memory-d36
MAP=.docs/plans/2026-04-29-p1-task-map.txt

# Read pairs (i, id), skip comment/empty lines
mapfile -t LINES < <(grep -E "^[0-9]+ " "$MAP")

PREV_ID=""
for line in "${LINES[@]}"; do
  i=$(echo "$line" | awk '{print $1}')
  id=$(echo "$line" | awk '{print $2}')

  # parent-child to epic
  bd link "$id" "$EPIC" --type=parent-child >/dev/null 2>&1 && echo "P1.$i -> parent=$EPIC"

  # blocks chain: $id blocks PREV_ID? No — PREV_ID blocks $id (current depends on prev)
  # bd link <issue> <depends-on> --type=blocks  means issue depends on (is blocked by) depends-on
  if [ -n "$PREV_ID" ]; then
    bd link "$id" "$PREV_ID" --type=blocks >/dev/null 2>&1 && echo "  P1.$i blocked by P1.$(printf "%02d" $((10#$i - 1)))"
  fi
  PREV_ID="$id"
done

echo ""
echo "=== bd ready (should show only P1.01) ==="
bd ready
