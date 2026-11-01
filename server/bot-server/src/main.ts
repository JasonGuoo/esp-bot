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
type PairState = "pending" | "approved" | "denied";
type PairingDevice = {
  device_id: string;
  state: PairState;
  name: string;
  first_seen_ms: number;
  approved_at_ms: number | null;
};
type PairingList = { devices: PairingDevice[] };
type ProviderStatus = {
  provider: string;
  configured: boolean;
  openai_base_url: string | null;
  s2s_url: string | null;
  health_ok: boolean;
  health_detail: string;
};
type ChatResult = { text: string; command_id: string | null; send_error?: string };
type ChatEntry = { role: "you" | "bot" | "error"; text: string; at_ms: number };

/** `?mock=1` renders representative fake data so the UI is previewable in a
 *  plain browser (`npm run dev` → http://localhost:1420/?mock=1). */
const MOCK = new URLSearchParams(location.search).has("mock");

if (navigator.userAgent.includes("Mac")) document.body.classList.add("platform-mac");

const history: LanEvent[] = [];
const chatLog: ChatEntry[] = [];
let activeBotId: string | null = localStorage.getItem("espbot.activeBot");
let lastSnapshot: Snapshot | null = null;
let currentView: "chat" | "events" = "chat";

const $ = <T extends HTMLElement>(selector: string): T => document.querySelector<T>(selector)!;
const ledEl = $<HTMLElement>("#led");
const serverLineEl = $<HTMLElement>("#server-line");
const devicesEl = $<HTMLElement>("#devices");
const devicesCountEl = $<HTMLElement>("#devices-count");
const pairingEl = $<HTMLElement>("#pairing");
const pairingCountEl = $<HTMLElement>("#pairing-count");
const providerBoxEl = $<HTMLElement>("#provider-box");
const targetEl = $<HTMLElement>("#chat-target");
const chatLogEl = $<HTMLElement>("#chat-log");
const chatErrorEl = $<HTMLElement>("#chat-error");
const chatForm = $<HTMLFormElement>("#chat-form");
const chatInput = $<HTMLInputElement>("#chat-input");
const eventsEl = $<HTMLElement>("#events");
const viewChat = $<HTMLElement>("#view-chat");
const viewEvents = $<HTMLElement>("#view-events");

function el<K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text = ""): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
}

function button(label: string, onClick: () => void, className = "", title = ""): HTMLButtonElement {
  const node = el("button", className, label);
  node.type = "button";
  if (title) node.title = title;
  node.addEventListener("click", () => void onClick());
  return node;
}

async function invokeX<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (MOCK) return mockCommand(command, args) as T;
  return invoke<T>(command, args);
}

// ── server status ─────────────────────────────────────
function renderServer(snapshot: Snapshot): void {
  const healthy = snapshot.running && !snapshot.error;
  ledEl.classList.toggle("on", healthy);
  ledEl.classList.toggle("err", Boolean(snapshot.error));
  if (snapshot.error) {
    serverLineEl.textContent = snapshot.error;
  } else if (healthy && snapshot.port !== null) {
    serverLineEl.textContent = `advertising · :${snapshot.port}`;
  } else {
    serverLineEl.textContent = "starting…";
  }
  serverLineEl.title = `server id ${snapshot.server_id}`;
}

// ── devices ───────────────────────────────────────────
function renderDevices(snapshot: Snapshot): void {
  lastSnapshot = snapshot;
  devicesEl.replaceChildren();
  devicesCountEl.hidden = snapshot.bots.length === 0;
  devicesCountEl.textContent = String(snapshot.bots.length);
  if (!snapshot.bots.length) {
    devicesEl.appendChild(el("p", "side-empty", "No bots connected"));
  }
  for (const bot of snapshot.bots) {
    const row = el("div", "device");
    if (bot.device_id === activeBotId) row.classList.add("selected");
    row.title = `${bot.name} · ${bot.peer} · since ${new Date(bot.connected_at_ms).toLocaleTimeString()}`;
    row.appendChild(el("span", "dot"));
    const meta = el("div", "meta");
    meta.appendChild(el("strong", "", bot.name));
    meta.appendChild(el("span", "", bot.device_id));
    row.appendChild(meta);
    row.appendChild(el("span", "link", "→"));
    row.addEventListener("click", () => {
      activeBotId = bot.device_id;
      localStorage.setItem("espbot.activeBot", activeBotId);
      renderDevices(snapshot);
      renderTarget();
    });
    devicesEl.appendChild(row);
  }
  if (activeBotId && !snapshot.bots.some((bot) => bot.device_id === activeBotId)) {
    activeBotId = null;
    localStorage.removeItem("espbot.activeBot");
  }
  renderTarget();
}

function renderTarget(): void {
  const bot = lastSnapshot?.bots.find((item) => item.device_id === activeBotId);
  targetEl.classList.toggle("none", !bot);
  if (!bot) {
    targetEl.replaceChildren(el("span", "", "No device selected"));
    return;
  }
  targetEl.replaceChildren(el("span", "", bot.name), el("span", "sub", bot.device_id));
}

// ── pairing ───────────────────────────────────────────
async function refreshPairing(): Promise<void> {
  let list: PairingList;
  try {
    list = await invokeX<PairingList>("pairing_list");
  } catch (error) {
    pairingEl.replaceChildren(el("p", "side-empty", `registry error: ${String(error)}`));
    return;
  }
  pairingEl.replaceChildren();
  if (!list.devices.length) {
    pairingEl.appendChild(el("p", "side-empty", "No devices known"));
    return;
  }
  const order: Record<PairState, number> = { pending: 0, approved: 1, denied: 2 };
  const devices = [...list.devices].sort(
    (a, b) => order[a.state] - order[b.state] || a.device_id.localeCompare(b.device_id),
  );
  const pending = devices.filter((device) => device.state === "pending").length;
  pairingCountEl.hidden = pending === 0;
  pairingCountEl.textContent = String(pending);
  for (const device of devices) {
    const row = el("div", `pair ${device.state}`);
    const meta = el("div", "meta");
    meta.appendChild(el("strong", "", device.name || device.device_id));
    meta.appendChild(el("span", "", device.device_id));
    row.appendChild(meta);
    if (device.state === "pending") {
      const actions = el("div", "actions");
      actions.appendChild(button("Approve", () => void pairingAct("pairing_approve", device.device_id), "btn tiny primary", "Allow commands to this device"));
      actions.appendChild(button("Deny", () => void pairingAct("pairing_deny", device.device_id), "btn tiny danger", "Refuse and disconnect"));
      row.appendChild(actions);
    } else {
      const wrap = el("div", "actions");
      wrap.appendChild(el("span", `tag ${device.state}`, device.state));
      wrap.appendChild(button("Forget", () => void pairingAct("pairing_remove", device.device_id), "btn tiny ghost", "Remove from registry"));
      row.appendChild(wrap);
    }
    pairingEl.appendChild(row);
  }
}

async function pairingAct(command: string, deviceId: string): Promise<void> {
  try {
    await invokeX(command, { deviceId });
  } catch (error) {
    addEvent({ kind: "ui_error", device_id: deviceId, detail: String(error), at_ms: Date.now() });
  }
  await refreshPairing();
}

// ── provider ──────────────────────────────────────────
function renderProvider(status: ProviderStatus): void {
  providerBoxEl.replaceChildren();
  const row = el("div", "row");
  const name = el("span", `name${status.configured ? "" : " off"}`, status.provider + (status.configured ? "" : " · off"));
  row.appendChild(name);
  row.appendChild(button("Reload", () => void reloadProvider(), "btn tiny ghost", "Re-read config.json"));
  const detail = el("p", `detail ${status.health_ok ? "ok" : "bad"}`, status.health_detail);
  providerBoxEl.appendChild(row);
  providerBoxEl.appendChild(detail);
}

async function reloadProvider(): Promise<void> {
  try {
    renderProvider(await invokeX<ProviderStatus>("provider_reload"));
  } catch (error) {
    addEvent({ kind: "ui_error", device_id: null, detail: String(error), at_ms: Date.now() });
  }
}

async function refreshProvider(): Promise<void> {
  try {
    renderProvider(await invokeX<ProviderStatus>("provider_status"));
  } catch (error) {
    providerBoxEl.replaceChildren();
    providerBoxEl.appendChild(el("p", "detail bad", `status unavailable: ${String(error)}`));
  }
}

// ── events view ───────────────────────────────────────
function kindClass(kind: string): string {
  if (kind.startsWith("command")) return "k-command";
  if (kind.startsWith("pairing")) return "k-pairing";
  if (kind.startsWith("server") || kind.includes("error")) return "k-server";
  if (kind.startsWith("bot")) return "k-bot";
  return "";
}

function renderEvents(): void {
  eventsEl.replaceChildren();
  if (!history.length) {
    eventsEl.appendChild(el("p", "events-empty", "No events yet"));
    return;
  }
  for (const event of history) {
    const row = el("div", "ev");
    row.appendChild(el("time", "", new Date(event.at_ms).toLocaleTimeString()));
    const kind = el("span", `kind ${kindClass(event.kind)}`, event.kind.replaceAll("_", " "));
    row.appendChild(kind);
    const detail = el("span", "detail");
    if (event.device_id) detail.appendChild(el("b", "", event.device_id));
    detail.appendChild(document.createTextNode(event.detail));
    row.appendChild(detail);
    eventsEl.appendChild(row);
  }
}

function addEvent(event: LanEvent): void {
  history.unshift(event);
  if (history.length > 50) history.pop();
  if (currentView === "events") renderEvents();
}

// ── chat ──────────────────────────────────────────────
function showChatError(message: string | null): void {
  chatErrorEl.hidden = message === null;
  chatErrorEl.textContent = message ?? "";
}

function renderChatLog(): void {
  chatLogEl.replaceChildren();
  if (!chatLog.length) {
    const empty = el("div", "chat-empty");
    const logo = el("span", "logo");
    logo.appendChild(el("i"));
    logo.appendChild(el("i"));
    empty.appendChild(logo);
    empty.appendChild(el("strong", "", "Chat with 小呆"));
    empty.appendChild(el("p", "", "Select a connected device, then type a message."));
    chatLogEl.appendChild(empty);
    return;
  }
  for (const entry of chatLog.slice(-60)) {
    const msg = el("div", `msg ${entry.role}`);
    msg.appendChild(el("div", "bubble", entry.text));
    msg.appendChild(el("span", "meta", new Date(entry.at_ms).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })));
    chatLogEl.appendChild(msg);
  }
  chatLogEl.scrollTop = chatLogEl.scrollHeight;
}

async function sendChat(message: string): Promise<void> {
  showChatError(null);
  if (!activeBotId) {
    showChatError("Select a connected device first.");
    return;
  }
  chatLog.push({ role: "you", text: message, at_ms: Date.now() });
  renderChatLog();
  try {
    const result = await invokeX<ChatResult>("chat", { deviceId: activeBotId, message });
    chatLog.push({ role: "bot", text: result.text, at_ms: Date.now() });
    renderChatLog();
    if (result.send_error) {
      showChatError(`LLM replied, but the bot did not receive it: ${result.send_error}`);
    }
  } catch (error) {
    // Backend errors carry honest codes (llm_not_configured, …); never fake a reply.
    showChatError(String(error));
  }
}

async function sendTestFace(): Promise<void> {
  showChatError(null);
  if (!activeBotId) {
    showChatError("Select a connected device first.");
    return;
  }
  try {
    await invokeX("send_face_command", {
      deviceId: activeBotId,
      payload: { expression: "happy", subtitle: "Hello from the Mac brain" },
    });
  } catch (error) {
    showChatError(String(error));
  }
}

// ── view switch ───────────────────────────────────────
$("#view-switch").addEventListener("click", (event) => {
  const target = event.target as HTMLElement;
  const view = target.dataset.view as "chat" | "events" | undefined;
  if (!view || view === currentView) return;
  currentView = view;
  const buttons = document.querySelectorAll<HTMLButtonElement>("#view-switch button");
  for (let i = 0; i < buttons.length; i += 1) {
    buttons[i].classList.toggle("on", buttons[i].dataset.view === view);
  }
  viewChat.hidden = view !== "chat";
  viewEvents.hidden = view !== "events";
  if (view === "events") renderEvents();
});

// ── wiring ────────────────────────────────────────────
chatForm.addEventListener("submit", (event) => {
  event.preventDefault();
  const message = chatInput.value.trim();
  if (!message) return;
  chatInput.value = "";
  void sendChat(message);
});
$("#chat-test").addEventListener("click", () => void sendTestFace());

async function refresh(): Promise<void> {
  try {
    const snapshot = await invokeX<Snapshot>("lan_snapshot");
    renderServer(snapshot);
    renderDevices(snapshot);
  } catch (error) {
    ledEl.classList.add("err");
    serverLineEl.textContent = `console error: ${String(error)}`;
  }
}

function wireLive(): void {
  void listen<LanEvent>("lan:event", (event) => {
    addEvent(event.payload);
    void refresh();
    if (event.payload.kind.startsWith("pairing")) void refreshPairing();
    if (event.payload.kind === "provider_loaded" || event.payload.kind === "config_error") void refreshProvider();
  });
  void refresh();
  void refreshPairing();
  void refreshProvider();
  window.setInterval(() => { void refresh(); }, 1500);
}

function wireMock(): void {
  $<HTMLElement>("#mock-badge").hidden = false;
  const now = Date.now();
  const snapshot: Snapshot = {
    running: true,
    service_type: "_espbot-server._tcp.local.",
    server_id: "5ff31d463a8c44b3ba409dbd86d2f4a3",
    instance_name: "ESP Bot Server 5ff31d46",
    port: 57773,
    bots: [
      { device_id: "korvo-01", name: "小呆 · Desk Bot", peer: "192.168.1.42", connected_at_ms: now - 62_000 },
    ],
    error: null,
  };
  renderServer(snapshot);
  renderDevices(snapshot);
  renderProvider({
    provider: "openai",
    configured: true,
    openai_base_url: "http://127.0.0.1:8080/v1",
    s2s_url: null,
    health_ok: true,
    health_detail: "HTTP 200 · mlx-lm @ 127.0.0.1:8080",
  });
  for (const [kind, device, detail, ago] of [
    ["pairing_pending", "korvo-01", "new device waiting for approval", 62],
    ["bot_connected", "korvo-01", "192.168.1.42:57778", 62],
    ["pairing_resolved", "korvo-01", "approved", 45],
    ["command_sent", "korvo-01", "face.set", 12],
    ["command_acked", "korvo-01", "c1 face.set applied", 12],
  ] as const) {
    history.push({ kind, device_id: device, detail, at_ms: now - ago * 1000 });
  }
  renderEvents();
  chatLog.push(
    { role: "you", text: "小呆小呆，今天天气怎么样？", at_ms: now - 40_000 },
    { role: "bot", text: "这边没法上网查天气，不过可以陪你聊聊天呀。", at_ms: now - 39_000 },
  );
  renderChatLog();
  // Fake pairing registry so the pending-approval row is previewable.
  pairingEl.replaceChildren();
  const row = el("div", "pair pending");
  const meta = el("div", "meta");
  meta.appendChild(el("strong", "", "Lobby Bot"));
  meta.appendChild(el("span", "", "esp-lobby-02"));
  row.appendChild(meta);
  const actions = el("div", "actions");
  actions.appendChild(button("Approve", () => {}, "btn tiny primary"));
  actions.appendChild(button("Deny", () => {}, "btn tiny danger"));
  row.appendChild(actions);
  pairingEl.appendChild(row);
  pairingCountEl.hidden = false;
  pairingCountEl.textContent = "1";
}

function mockCommand(command: string, args?: Record<string, unknown>): unknown {
  if (command === "chat") {
    const message = String(args?.message ?? "");
    return { text: `Mock reply: ${message}`, command_id: "c99" };
  }
  if (command === "send_face_command") return "c100";
  return null;
}

MOCK ? wireMock() : wireLive();
