import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import "./style.css";

type Bot = { device_id: string; name: string; peer: string; connected_at_ms: number };
type Snapshot = {
  running: boolean;
  service_type: string;
  server_id: string;
  instance_name: string;
  port: number | null;
  bots: Bot[];
  error: string | null;
};
type LanEvent = { kind: string; device_id: string | null; detail: string; at_ms: number };
const history: LanEvent[] = [];
const statusEl = document.querySelector<HTMLElement>("#status")!;
const ledEl = document.querySelector<HTMLElement>("#led")!;
const devicesEl = document.querySelector<HTMLElement>("#devices")!;
const eventsEl = document.querySelector<HTMLElement>("#events")!;

function text(parent: HTMLElement, tag: string, value: string, className = ""): HTMLElement {
  const el = document.createElement(tag);
  el.className = className;
  el.textContent = value;
  parent.appendChild(el);
  return el;
}

function render(snapshot: Snapshot): void {
  const healthy = snapshot.running && !snapshot.error;
  ledEl.classList.toggle("on", healthy);
  statusEl.textContent = snapshot.error ? "Error: " + snapshot.error : healthy ? "Advertising on LAN" : "Starting…";
  document.querySelector<HTMLElement>("#port")!.textContent =
    snapshot.port === null ? "—" : "0.0.0.0:" + snapshot.port;
  document.querySelector<HTMLElement>("#count")!.textContent = String(snapshot.bots.length);
  devicesEl.replaceChildren();
  if (!snapshot.bots.length) {
    text(devicesEl, "p", "Waiting for an ESP Bot to connect…", "empty");
  }
  for (const bot of snapshot.bots) {
    const item = text(devicesEl, "article", "", "device");
    const left = text(item, "div", "");
    text(left, "strong", bot.name);
    text(left, "p", bot.device_id + " · " + bot.peer);
    text(item, "time", new Date(bot.connected_at_ms).toLocaleTimeString());
  }
}

function addEvent(event: LanEvent): void {
  history.unshift(event);
  if (history.length > 40) history.pop();
  eventsEl.replaceChildren();
  for (const e of history) {
    const li = document.createElement("li");
    text(li, "time", new Date(e.at_ms).toLocaleTimeString());
    text(li, "strong", e.kind.replaceAll("_", " "));
    text(li, "span", (e.device_id ? e.device_id + " · " : "") + e.detail);
    eventsEl.appendChild(li);
  }
}

async function refresh(): Promise<void> {
  try {
    render(await invoke<Snapshot>("lan_snapshot"));
  } catch (error) {
    ledEl.classList.remove("on");
    statusEl.textContent = "Console error: " + String(error);
  }
}

void listen<LanEvent>("lan:event", (event) => {
  addEvent(event.payload);
  void refresh();
});
void refresh();
window.setInterval(() => { void refresh(); }, 1500);
