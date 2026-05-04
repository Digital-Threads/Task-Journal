#!/usr/bin/env node

/**
 * postinstall — ensures the Rust binaries (task-journal-cli, task-journal-mcp)
 * are available. If not found in PATH, installs them via `cargo install`.
 */

import { execSync } from "node:child_process";

function binExists(name) {
  try {
    execSync(`which ${name} 2>/dev/null || where ${name} 2>nul`, {
      stdio: "pipe",
    });
    return true;
  } catch {
    return false;
  }
}

const bins = [
  { crate: "task-journal-cli", bin: "task-journal" },
  { crate: "task-journal-mcp", bin: "task-journal-mcp" },
];

const missing = bins.filter((b) => !binExists(b.bin));

if (missing.length === 0) {
  console.log("✓ task-journal binaries already installed");
  process.exit(0);
}

// Check if cargo is available
try {
  execSync("cargo --version", { stdio: "pipe" });
} catch {
  console.error(
    [
      "",
      "╔══════════════════════════════════════════════════════════════╗",
      "║  task-journal requires Rust toolchain (cargo) to install    ║",
      "║  the MCP server and CLI binaries.                          ║",
      "║                                                            ║",
      "║  Install Rust: https://rustup.rs                           ║",
      "║                                                            ║",
      "║  Then run:                                                 ║",
      "║    cargo install task-journal-cli task-journal-mcp          ║",
      "╚══════════════════════════════════════════════════════════════╝",
      "",
    ].join("\n")
  );
  // Don't fail the npm install — the plugin structure is still valid,
  // the user just needs to install binaries manually.
  process.exit(0);
}

const crates = missing.map((b) => b.crate).join(" ");
console.log(`Installing ${crates} from crates.io...`);

try {
  execSync(`cargo install ${crates}`, { stdio: "inherit" });
  console.log("✓ task-journal binaries installed successfully");
} catch (err) {
  console.error(
    `\n⚠ Failed to install binaries. Run manually:\n  cargo install ${crates}\n`
  );
  // Non-fatal — don't break npm install
  process.exit(0);
}
