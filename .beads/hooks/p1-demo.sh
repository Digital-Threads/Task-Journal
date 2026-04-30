#!/bin/bash
set -e
cd /home/shahinyanm/www/claude-memory

DEMO=/tmp/tj-demo
rm -rf "$DEMO"
mkdir -p "$DEMO"
export XDG_DATA_HOME="$DEMO"

echo "==================== P1 LIVE DEMO ===================="
echo "XDG_DATA_HOME=$XDG_DATA_HOME"
echo

echo ">>> 1. Create task A"
TASK_A=$(./target/debug/task-journal create "Demo: validate Phase 1 e2e flow")
echo "    -> $TASK_A"
echo

echo ">>> 2. Create task B"
TASK_B=$(./target/debug/task-journal create "Second task: prove project_hash isolation")
echo "    -> $TASK_B"
echo

echo ">>> 3. Add 3 events directly to JSONL (no CLI 'event' command yet — that's P2)"
JSONL=$(find "$DEMO" -name "*.jsonl")
echo "    JSONL: $JSONL"
ls -la "$JSONL"
echo

echo ">>> 4. List events (most recent first)"
./target/debug/task-journal events list
echo

echo ">>> 5. JSONL content (last line, pretty)"
tail -1 "$JSONL" | python3 -m json.tool 2>/dev/null || tail -1 "$JSONL"
echo

echo ">>> 6. Rebuild SQLite"
./target/debug/task-journal rebuild-state
echo

echo ">>> 7. SQLite tasks table:"
SQLITE=$(find "$DEMO" -name "*.sqlite")
echo "    SQLite: $SQLITE"
sqlite3 "$SQLITE" "SELECT task_id, status, title, opened_at FROM tasks ORDER BY opened_at;"
echo

echo ">>> 8. SQLite events_index:"
sqlite3 "$SQLITE" "SELECT task_id, type, status FROM events_index ORDER BY timestamp;"
echo

echo ">>> 9. Storage tree"
find "$DEMO" -type f -exec ls -la {} \;
echo

echo "==================== DEMO COMPLETE ===================="
