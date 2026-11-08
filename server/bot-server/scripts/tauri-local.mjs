// Use the locally selected signing identity without putting its certificate or
// private key in the repository. Explicit caller configuration takes priority.
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const env = { ...process.env };

if (process.platform === "darwin" && ["build", "bundle"].includes(args[0]) && !env.APPLE_SIGNING_IDENTITY) {
  try {
    const identity = readFileSync(resolve(root, "runtime/macos-signing-identity"), "utf8").trim();
    if (!/^[a-f0-9]{40}$/i.test(identity)) throw new Error("invalid local code-signing identity fingerprint");
    env.APPLE_SIGNING_IDENTITY = identity;
  } catch (error) {
    if (error?.code !== "ENOENT") {
      console.error(`Cannot use local code-signing identity: ${error.message}`);
      process.exit(1);
    }
  }
}

const cli = resolve(root, "node_modules/@tauri-apps/cli/tauri.js");
const result = spawnSync(process.execPath, [cli, ...args], { stdio: "inherit", env });
if (result.error) {
  console.error(`Cannot start Tauri CLI: ${result.error.message}`);
  process.exit(1);
}
process.exit(result.status ?? 1);
