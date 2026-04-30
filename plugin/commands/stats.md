---
description: Show local classifier and journal statistics
---

The user wants to see classifier statistics — how many events were auto-captured, what fraction are confirmed vs suggested, etc.

This is a CLI-only feature (no MCP tool yet). Run it via Bash:

```
task-journal stats
```

(On Windows + WSL: `wsl -d Ubuntu -- task-journal stats`.)

Print the output as-is to the user. Brief commentary if `confirmed ratio < 0.6` — suggest the classifier may be too aggressive or the prompt needs tuning.
