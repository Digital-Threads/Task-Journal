#!/bin/bash
set -e
cd /home/shahinyanm/www/claude-memory

DEMO=/tmp/tj-p4-demo
rm -rf "$DEMO"
mkdir -p "$DEMO"
export XDG_DATA_HOME="$DEMO"

BIN=./target/debug/task-journal

echo "==================== P4 DEMO (polish + dogfood) ===================="
echo "XDG_DATA_HOME=$DEMO"
echo

echo ">>> 1. Two projects via two project_hashes (synthesized)"
# Easier: just create two tasks in same cwd; project_hash is same.
# To exercise --all-projects we synthesize two state DBs by hand.
TASK_A=$($BIN create "Pick auth flow")
echo "    Task A: $TASK_A"
$BIN event "$TASK_A" --type decision --text "Adopt PKCE flow" >/dev/null
$BIN event "$TASK_A" --type rejection --text "Implicit grant: deprecated" >/dev/null

TASK_B=$($BIN create "Build pack assembler")
echo "    Task B: $TASK_B"
$BIN event "$TASK_B" --type decision --text "Use SQLite views" >/dev/null

echo
echo ">>> 2. Cross-project search (single project here, exercises the flag)"
$BIN search "PKCE" --all-projects | head -5
$BIN search "SQLite" --all-projects | head -5

echo
echo ">>> 3. Stats (no telemetry yet)"
$BIN stats

echo
echo ">>> 4. Mock-classifier ingest (writes telemetry)"
$BIN ingest-hook --kind Stop --text "decided to use Rust" \
  --mock-event-type decision --mock-task-id "$TASK_A" --mock-confidence 0.92 >/dev/null
$BIN ingest-hook --kind Stop --text "maybe consider scopes" \
  --mock-event-type constraint --mock-task-id "$TASK_A" --mock-confidence 0.6 >/dev/null
echo

echo ">>> 5. Stats (after telemetry)"
$BIN stats

echo
echo ">>> 6. Full pack with [?] marker for low-confidence event:"
echo "----------------------------------------"
$BIN pack "$TASK_A" --mode full
echo "----------------------------------------"

echo
echo ">>> 7. install-hooks dry-run (with || true wrapping):"
TMP_HOME=/tmp/tj-p4-home
rm -rf "$TMP_HOME"
HOME="$TMP_HOME" $BIN install-hooks --scope user >/dev/null
echo "Settings file:"
cat "$TMP_HOME/.claude/settings.json"

echo
echo ">>> 8. ingest-hook --help (verify mock flags hidden):"
$BIN ingest-hook --help

echo
echo "==================== DEMO COMPLETE ===================="
