#!/bin/bash
set -e
export PATH="$HOME/.local/bin:$PATH"
cd /home/shahinyanm/www/claude-memory

for id in claude-memory-4l6 claude-memory-djm claude-memory-gzl claude-memory-b7d claude-memory-2zi claude-memory-dz1; do
  echo "Closing $id"
  bd close "$id" --reason "slash command created" 2>&1 | tail -1
done

echo
echo "=== Verification ==="
for id in claude-memory-4l6 claude-memory-djm claude-memory-gzl claude-memory-b7d claude-memory-2zi claude-memory-dz1; do
  bd show "$id" 2>&1 | head -1
done
