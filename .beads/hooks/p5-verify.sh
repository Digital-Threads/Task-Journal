#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

echo "=================== P5 VERIFICATION ==================="
echo
echo "=== Plugin layout ==="
find plugin/ -type f | sort

echo
echo "=== plugin.json valid ==="
python3 -m json.tool plugin/.claude-plugin/plugin.json > /dev/null && echo "OK"

echo
echo "=== .mcp.json valid ==="
python3 -m json.tool plugin/.mcp.json > /dev/null && echo "OK"

echo
echo "=== Slash commands frontmatter ==="
for f in plugin/commands/*.md; do
  if head -1 "$f" | grep -q "^---$"; then
    echo "  OK: $(basename $f)"
  else
    echo "  FAIL: $(basename $f)"
  fi
done

echo
echo "=== SKILL.md frontmatter ==="
head -4 plugin/skills/task-journal/SKILL.md

echo
echo "=== Workspace tests still green ==="
cargo test --workspace 2>&1 | grep -E "^test result.*passed" | wc -l
echo "(should be 8 — one per test runner)"

echo
echo "=== plugin-validator (if available) ==="
# Show hooks block from manifest as proof
python3 -c "
import json
m = json.load(open('plugin/.claude-plugin/plugin.json'))
print('Hooks declared:', list(m.get('hooks', {}).keys()))
print('MCP server: see .mcp.json (separate file)')
print('Plugin name:', m['name'], 'version:', m['version'])
"

echo
echo "=== bd P5 closure ==="
for id in claude-memory-1y3 claude-memory-4ns; do
  echo -n "$id: "
  bd show "$id" 2>&1 | head -1
done
