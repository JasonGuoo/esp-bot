import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  // The console reuses the preview's face renderer from the repository root.
  server: {
    fs: { allow: [fileURLToPath(new URL("../..", import.meta.url))] },
    watch: {
      // These are backend outputs and model environments, not frontend sources.
      // Watching Cargo's target can otherwise scan tens of gigabytes on every poll.
      ignored: ["**/src-tauri/target/**", "**/bot-server/local-models/**", "**/bot-server/.venv-tools/**", "**/bot-server/runtime/**"],
      // On macOS without optional fsevents, chokidar otherwise defaults to 100 ms
      // polling. Native fs.watch is sufficient for the local source tree.
      ...(process.platform === "darwin" ? { usePolling: false } : {}),
    },
  },
  build: { rollupOptions: { input: {
    main: fileURLToPath(new URL("./index.html", import.meta.url)),
    usbDebug: fileURLToPath(new URL("./usb-debug.html", import.meta.url)),
  } } },
});
