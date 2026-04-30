#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

EPIC=claude-memory-d36
MAP=.docs/plans/2026-04-30-p3-task-map.txt
mapfile -t LINES < <(grep -E "^[0-9]+ " "$MAP")

PREV_ID=""
for line in "${LINES[@]}"; do
  i=$(echo "$line" | awk '{print $1}')
  id=$(echo "$line" | awk '{print $2}')
  bd link "$id" "$EPIC" --type=parent-child >/dev/null 2>&1 && echo "P3.$i -> parent=$EPIC"
  if [ -n "$PREV_ID" ]; then
    bd link "$id" "$PREV_ID" --type=blocks >/dev/null 2>&1 && \
      echo "  P3.$i blocked by P3.$(printf "%02d" $((10#$i - 1)))"
  fi
  PREV_ID="$id"
done

echo ""
bd ready
