import "./style.css";
import "./companion.css";
import "./console.css";
import { mountUsbDebug } from "./usb-debug";
const root = document.querySelector<HTMLElement>("#usb-debug-root");
if (!root) throw new Error("Missing USB debug root");
root.style.cssText = "padding:24px;max-width:1280px;margin:0 auto;height:100vh;overflow:auto";
const debug = mountUsbDebug(root);
window.addEventListener("pagehide", () => debug.dispose(), { once: true });
