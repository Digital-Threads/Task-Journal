#!/bin/bash
set -e
cd /home/shahinyanm/www/claude-memory

DEMO=/tmp/tj-p3-mock
rm -rf "$DEMO"
mkdir -p "$DEMO"
export XDG_DATA_HOME="$DEMO"

echo "==================== P3 MOCK-CLASSIFIER DEMO ===================="
echo "XDG_DATA_HOME=$DEMO"
echo

TASK_ID=$(./target/debug/task-journal create "P3 verification: hooks + classifier")
echo "Task: $TASK_ID"
./target/debug/task-journal event "$TASK_ID" --type hypothesis --text "PKCE vs implicit grant" >/dev/null

# High-confidence: status=confirmed, no [?]
./target/debug/task-journal ingest-hook \
  --kind Stop \
  --text "After review I am going to adopt PKCE for OAuth flow." \
  --mock-event-type decision \
  --mock-task-id "$TASK_ID" \
  --mock-confidence 0.92 >/dev/null
echo "+ confirmed (conf=0.92) -> decision"

# Low-confidence: status=suggested, will show [?]
./target/debug/task-journal ingest-hook \
  --kind Stop \
  --text "Maybe we should consider scopes carefully." \
  --mock-event-type constraint \
  --mock-task-id "$TASK_ID" \
  --mock-confidence 0.65 >/dev/null
echo "+ suggested (conf=0.65) -> constraint"

# Manual correction event
./target/debug/task-journal ingest-hook \
  --kind PostToolUse \
  --text "Migration looks complete (was wrong)" \
  --mock-event-type finding \
  --mock-task-id "$TASK_ID" \
  --mock-confidence 0.88 >/dev/null

# Find that finding's id, then correct it
LAST_FINDING_ID=$(tail -1 "$DEMO/task-journal/events/"*.jsonl | python3 -c "import sys, json; print(json.loads(sys.stdin.read())['event_id'])" 2>/dev/null)
if [ -n "$LAST_FINDING_ID" ]; then
  ./target/debug/task-journal event-correct \
    --corrects "$LAST_FINDING_ID" \
    --task "$TASK_ID" \
    --text "Migration was NOT done; finding was wrong" >/dev/null
  echo "+ correction event linked to $LAST_FINDING_ID"
fi

echo
echo ">>> Full pack:"
echo "----------------------------------------"
./target/debug/task-journal pack "$TASK_ID" --mode full
echo "----------------------------------------"
echo
echo ">>> install-hooks dry-run on tmp HOME:"
TMP_HOME=/tmp/tj-p3-home
rm -rf "$TMP_HOME"
HOME="$TMP_HOME" ./target/debug/task-journal install-hooks --scope user >/dev/null
echo "Settings file:"
cat "$TMP_HOME/.claude/settings.json"
