#!/bin/bash
set -e
cd /home/shahinyanm/www/claude-memory

DEMO=/tmp/tj-p2-demo
rm -rf "$DEMO"
mkdir -p "$DEMO"
export XDG_DATA_HOME="$DEMO"

echo "==================== P2 LIVE DEMO ===================="
echo "XDG_DATA_HOME=$XDG_DATA_HOME"
echo

echo ">>> 1. Create task"
TASK_ID=$(./target/debug/task-journal create "P2 verification: real task_pack")
echo "    -> $TASK_ID"
echo

echo ">>> 2. Add events"
./target/debug/task-journal event "$TASK_ID" --type hypothesis --text "Pack assembler renders sections correctly" >/dev/null
echo "    + hypothesis"
./target/debug/task-journal event "$TASK_ID" --type evidence --text "Golden fixture B passes (12 events)" >/dev/null
echo "    + evidence"
./target/debug/task-journal event "$TASK_ID" --type decision --text "Adopt FTS5 for search" >/dev/null
echo "    + decision"
./target/debug/task-journal event "$TASK_ID" --type rejection --text "JSON LIKE: too slow at scale" >/dev/null
echo "    + rejection"
./target/debug/task-journal close "$TASK_ID" --reason "shipped" >/dev/null
echo "    + close"
echo

echo ">>> 3. Compact pack:"
echo "----------------------------------------"
./target/debug/task-journal pack "$TASK_ID" --mode compact
echo "----------------------------------------"
echo

echo ">>> 4. Full pack:"
echo "----------------------------------------"
./target/debug/task-journal pack "$TASK_ID" --mode full
echo "----------------------------------------"
echo

echo ">>> 5. Search 'FTS5':"
./target/debug/task-journal search "FTS5"
echo

echo ">>> 6. Search 'Pack':"
./target/debug/task-journal search "Pack"
echo

echo "==================== DEMO COMPLETE ===================="
