import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  // The console reuses the preview's face renderer from the repository root.
  server: { fs: { allow: [fileURLToPath(new URL("../..", import.meta.url))] } },
  build: { rollupOptions: { input: {
    main: fileURLToPath(new URL("./index.html", import.meta.url)),
    usbDebug: fileURLToPath(new URL("./usb-debug.html", import.meta.url)),
  } } },
});
