#!/bin/bash
set -e
cd /home/shahinyanm/www/claude-memory

if [ -z "$ANTHROPIC_API_KEY" ]; then
  echo "ANTHROPIC_API_KEY not set; skipping real-API smoke."
  echo "(export ANTHROPIC_API_KEY=sk-ant-... and rerun to exercise the live classifier path)"
  exit 0
fi

DEMO=/tmp/tj-p3-demo
rm -rf "$DEMO"
mkdir -p "$DEMO"
export XDG_DATA_HOME="$DEMO"

echo "==================== P3 LIVE DEMO (real Anthropic) ===================="
echo "XDG_DATA_HOME=$DEMO"
echo

TASK_ID=$(./target/debug/task-journal create "Pick auth flow")
echo "Task: $TASK_ID"
./target/debug/task-journal event "$TASK_ID" --type hypothesis --text "PKCE vs implicit grant" >/dev/null
echo "+ hypothesis"

echo ""
echo ">>> Simulate a Stop hook: assistant stated a decision"
./target/debug/task-journal ingest-hook \
  --kind Stop \
  --text "After review I'm going to adopt PKCE for the OAuth flow because OAuth 2.1 deprecates implicit."

echo ""
echo ">>> Resulting full pack:"
./target/debug/task-journal pack "$TASK_ID" --mode full
