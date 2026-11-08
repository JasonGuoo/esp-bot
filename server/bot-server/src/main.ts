import "./style.css";
import { mountCompanion } from "./companion";
import { mountUsbDebug } from "./usb-debug";

const container = document.querySelector<HTMLElement>("#view-companion");
if (!container) throw new Error("Missing companion root");
const companion = mountCompanion(container);
const devices = container.querySelector<HTMLElement>("#cc-panel-devices");
const usbDebug = devices ? mountUsbDebug(devices) : null;
document.addEventListener("visibilitychange", () => { companion.setVisible(!document.hidden); });
window.addEventListener("pagehide", () => { companion.dispose(); usbDebug?.dispose(); }, { once: true });
