#!/usr/bin/env bun
import { $ } from "zx";
import { existsSync, unlinkSync } from "fs";

$.verbose = true;

const socketPath = "/run/user/1000/wayland-proxy-9";

// Clean up stale socket if any
if (existsSync(socketPath)) {
  try {
    unlinkSync(socketPath);
  } catch (e) {}
}

console.log("Starting sommelier proxy server...");
// Spawn sommelier in the background
const sommelierProc = $`nix develop --command _local/sommelier-rs/target/debug/sommelier wayland-proxy-9`.nothrow();

// Wait for socket to be created (max 10 seconds)
let retries = 100;
while (retries > 0 && !existsSync(socketPath)) {
  await new Promise((r) => setTimeout(r, 100));
  retries--;
}

if (!existsSync(socketPath)) {
  console.error("Error: sommelier proxy socket was not created in time.");
  sommelierProc.kill("SIGTERM");
  process.exit(1);
}

console.log("Sommelier proxy socket created successfully. Launching sommelier-test-gui...");

try {
  // Run the test gui with auto-exit under the proxy display
  const guiResult = await $`WAYLAND_DISPLAY=wayland-proxy-9 nix develop --command _local/sommelier-rs/target/debug/sommelier-test-gui --auto-exit`;
  
  if (guiResult.exitCode === 0) {
    console.log("End-to-end integration test completed successfully!");
    process.exit(0);
  } else {
    console.error(`sommelier-test-gui exited with non-zero code: ${guiResult.exitCode}`);
    process.exit(1);
  }
} catch (error) {
  console.error("Test GUI failed to execute:", error);
  process.exit(1);
} finally {
  // Clean up processes and socket
  sommelierProc.kill("SIGTERM");
  if (existsSync(socketPath)) {
    try {
      unlinkSync(socketPath);
    } catch (e) {}
  }
}
