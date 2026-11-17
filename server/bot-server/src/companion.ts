import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { LocalCamera, PcmRecorder, base64ToBuffer, readImageFile, readWavFile, type CapturedWav } from "./capture";
import { CONSOLE_PANELS, consoleTemplate, type ConsolePanel } from "./console-layout";
import { mountHousehold, readArtifactContent } from "./household";
import { deviceDeliveryText, deviceReplyMetrics, renderActivity, renderCapabilities, renderDeviceStatus, renderRuntimeMetrics, renderTurnTimings, turnStageMetric, harnessTimingMetric, type DeviceDisplayReport } from "./console-presenters";
import "./companion.css";
import "./console.css";

type JsonObject = Record<string, unknown>;
type DeviceTurnView = { id: string; device: string; text: DeviceDisplayReport; image: DeviceDisplayReport };
type Profile = { id: string; display_name: string; is_child: boolean; merged_into?: string | null };
type MemoryRecord = {
  id: string; scope: "household" | "profile"; profile_id: string | null; content: string;
  category: string; source: string; source_ref: string | null; confirmed: boolean;
  allow_cloud: boolean; expires_at_ms: number | null;
};
type UnattributedMemory = { id: string; content: string; category: string; quote: string; created_at_ms: number };
type Enrollment = { profile_id: string; kind: string; model_id: string; sample_count: number };
type Card = { kind: string; title: string; body: string; url: string | null };
type Scene = { speech: string; expression: string; cards: Card[] };
type TurnResult = {
  turn_id: string; conversation_id: string; transcript: string; scene: Scene;
  provider_id: string; model_id: string; audio: unknown; identity_candidate: unknown; warnings: string[];
  timings?: unknown;
  journal?: unknown;
  presentation?: unknown;
  task_ids?: string[];
};
type IdentityKind = "face" | "speaker";
type CapturePurpose = "chat" | "enroll" | "recognize";
type Sample = { base64: string; label: string };
type Phase = "idle" | "listening" | "thinking" | "speaking" | "error";

export type CompanionController = { setVisible(visible: boolean): void; dispose(): void };

function element<K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text = ""): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text) node.textContent = text;
  return node;
}

function object(value: unknown): JsonObject {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as JsonObject : {};
}
function string(value: unknown, fallback = ""): string { return typeof value === "string" ? value : fallback; }
function list(value: unknown): unknown[] { return Array.isArray(value) ? value : []; }
function number(value: unknown, fallback = 0): number { return typeof value === "number" && Number.isFinite(value) ? value : fallback; }
function pretty(value: unknown, max = 20_000): string {
  const encoded = JSON.stringify(value, null, 2) ?? String(value);
  return encoded.length <= max ? encoded : `${encoded.slice(0, max)}\n…（内容过长，已截断）`;
}
function newId(prefix: string): string {
  if (typeof crypto.randomUUID === "function") return `${prefix}_${crypto.randomUUID()}`;
  const bytes = crypto.getRandomValues(new Uint8Array(16));
  return `${prefix}_${Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")}`;
}
function errorText(error: unknown): string {
  const text = String(error);
  if (text.includes("capture_cancelled")) return "已取消采集。";
  for (const [code, message] of [
    ["brain_not_started", "服务尚未启动，请点击左下角“启动服务”。"],
    ["all_routes_unavailable", "当前没有可调用的模型路线。请在“模型与密钥”检查密钥、路线开关和冷却状态。"],
    ["local_worker_not_configured", "本地模型环境尚未配置，请在“本地能力”检查 Python 与 Worker 路径。"],
    ["no_speech", "这段录音没有识别出文字。等小呆提示“请说”后再说一次。"],
    ["audio_too_quiet", "小呆收到的声音太轻。请靠近麦克风，等“请说”后再试。"],
    ["brain_busy", "还有一个任务正在进行，可以先停止当前任务再重试。"],
    ["device_volume_unavailable", "设备未连接或暂不支持远程音量控制。"],
    ["device_settings_open", "请先关闭小呆上的设置页面。"],
    ["device_volume_busy", "上一次音量调整还在等待设备确认。"],
    ["api_key_keychain_authorization_timeout", "等待 macOS 钥匙串授权已超时，密钥仍然保留。请完成系统授权，再点击“启动服务”。"],
    ["api_key_keychain_read", "macOS 未允许读取已保存密钥。请解锁登录钥匙串并完成系统授权，再启动服务。无需重新填写密钥。"],
  ]) if (text.includes(code)) return message;
  return text.slice(0, 2000);
}

function captureErrorText(error: unknown): string {
  if (error instanceof DOMException && error.name === "NotAllowedError") return "没有获得麦克风或摄像头权限。可在 macOS 设置里开启，或选择本地文件。";
  if (error instanceof DOMException && error.name === "NotFoundError") return "没有找到可用的麦克风或摄像头。可选择本地文件继续。";
  return errorText(error);
}

export function mountCompanion(container: HTMLElement): CompanionController {
  // Static application template only. Every model, file, profile and tool value
  // below is assigned with textContent / DOM properties, never interpolated HTML.
  container.innerHTML = consoleTemplate;

  const $ = <T extends HTMLElement>(selector: string): T => {
    const node = container.querySelector<T>(selector);
    if (!node) throw new Error(`Missing companion element ${selector}`);
    return node;
  };
  const input = $<HTMLTextAreaElement>("#cc-input");
  const profileSelect = $<HTMLSelectElement>("#cc-profile");
  const privateScope = $<HTMLInputElement>("#cc-private-scope");
  const memoryOwner = $<HTMLSelectElement>("#cc-memory-owner");
  const identityKind = $<HTMLSelectElement>("#cc-identity-kind");
  const recognizeKind = $<HTMLSelectElement>("#cc-recognize-kind");
  const keyRoute = $<HTMLSelectElement>("#cc-api-key-route");
  const keyInput = $<HTMLInputElement>("#cc-api-key-value");
  const memberFocus = $<HTMLSelectElement>("#cc-member-focus");
  const memoryDialog = $<HTMLDialogElement>("#cc-memory-dialog");
  const profileDialog = $<HTMLDialogElement>("#cc-profile-dialog");
  const messages = $("#cc-messages");
  const recorder = new PcmRecorder();
  const camera = new LocalCamera();
  const cameraDialog = $<HTMLDialogElement>("#cc-camera-dialog");
  let visible = true;
  let disposed = false;
  let panel: ConsolePanel = "talk";
  let profiles: Profile[] = [];
  let memories: MemoryRecord[] = [];
  let unattributedMemories: UnattributedMemory[] = [];
  let enrollments: Enrollment[] = [];
  let selectedProfile: string | null = null;
  let conversationId = newId("console");
  let activeTurn: string | null = null;
  let activeDeviceTurn: DeviceTurnView | null = null;
  let resultDeviceTurn: DeviceTurnView | null = null;
  const resultDelivery = element("p", "cc-caption");
  const deviceToolEvents = new Set<string>();
  let turnGeneration = 0;
  let mediaGeneration = 0;
  let turnBusy = false;
  let backendCancellable = false;
  let householdPlaying = false;
  let stopBusy = false;
  let operationBusy = false;
  let statusBusy = false;
  let statusAgain = false;
  let permissionBusy = false;
  const permissionCards = new Map<string, { card: HTMLElement; signature: string }>();
  const livePermissions = new Map<string, string>();
  const permissionReplies = new Set<string>();
  let identityOperation = 0;
  let questionBusy = false;
  let questionSignature = "";
  let pendingQuestionCount = 0;
  let serviceReady = false;
  let serviceRunning = false;
  let speakerStates: JsonObject[] = [];
  let speakerDirty = false;
  let speakerBusy = false;
  let speakerSelectedId = "";
  let recordPurpose: CapturePurpose | null = null;
  let captureConverting = false;
  let cameraPurpose: "enroll" | "recognize" = "enroll";
  let cameraRequestGeneration = 0;
  let wavDraft: CapturedWav | null = null;
  let samples: Sample[] = [];
  let editingMemory: MemoryRecord | null = null;
  let minSamples = 3;
  let maxSamples = 12;
  let approvedMediaHosts = new Set<string>();
  let audio: HTMLAudioElement | null = null;
  let audioUrl: string | null = null;
  const replayClips = new Map<string, { base64: string; artifactIds: string[]; control: HTMLButtonElement }>();
  let unsubscribe: UnlistenFn | null = null;
  let credentialRoutes: JsonObject[] = [];
  let loadedConfiguration: JsonObject | null = null;
  let rawConfigDirty = false;
  let routeFormDirty = false;
  let editingRouteId = "";
  let editingProfileId: string | null = null;
  let unsubscribeLan: UnlistenFn | null = null;
  const activity: { label: string; at: number }[] = [];
  const dirtyConfigurationForms = new Set<string>();
  let pendingPermissionCount = 0;
  let latestModelHealth: JsonObject | null = null;

  const request = async <T = unknown>(method: string, payload: unknown = {}): Promise<T> => {
    return invoke<T>("companion_request", { method, payload });
  };
  const householdController = mountHousehold($("#cc-panel-household"), $("#cc-panel-tasks"), {
    request, notice, profiles: () => profiles, onProfilesChange: refreshFamily,
    onPlaybackChange: (playing) => { householdPlaying = playing; updateControls(); },
  });

  function notice(text: string, error = false): void {
    if (disposed) return;
    const node = $("#cc-notice");
    $("#cc-notice-wrap").hidden = !text;
    $("#cc-notice-wrap").classList.toggle("error", error);
    node.hidden = !text;
    node.classList.toggle("error", error);
    node.setAttribute("role", error ? "alert" : "status");
    node.setAttribute("aria-live", error ? "assertive" : "polite");
    node.textContent = text;
  }
  function phase(value: Phase, description?: string, expression?: string): void {
    const defaults: Record<Phase, string> = {
      idle: "聊一聊，或者一起发现一个小原理。", listening: "正在录音，只听你这一次主动说的话。",
      thinking: "我想一想，也查一查需要的资料…", speaking: "小呆正在说话。", error: "这一步没完成，可以看看提示。",
    };
    $("#cc-phase").textContent = description ?? defaults[value];
    $("#cc-phase").hidden = panel !== "talk" && value === "idle";
    $("#cc-phase").setAttribute("aria-live", value === "listening" ? "off" : "polite");
    const allowed = new Set(["idle", "happy", "curious", "thinking", "surprised", "sleepy", "shy", "listening", "speaking"]);
    const chosen = expression ?? (value === "error" ? "curious" : value);
    $("#cc-pet").dataset.expression = allowed.has(chosen) ? chosen : "idle";
  }
  function profileName(id: string | null): string {
    return id ? profiles.find((profile) => profile.id === id)?.display_name ?? `成员档案 ${id.slice(-6)}` : "家庭共享";
  }
  function button(label: string, action: () => void | Promise<void>, className = "cc-button quiet"): HTMLButtonElement {
    const node = element("button", className, label);
    node.type = "button";
    node.addEventListener("click", () => { void action(); });
    return node;
  }
  function activatePanel(next: string): void {
    const page = CONSOLE_PANELS.find((candidate) => candidate.id === next);
    if (!page) return;
    if (panel !== next) {
      cancelCapture(); closeCamera(); keyInput.value = ""; if (panel === "devices") clearToken();
      if (!operationBusy && !turnBusy) notice("");
    }
    panel = page.id;
    for (const tab of Array.from(container.querySelectorAll<HTMLButtonElement>(".cc-nav-item"))) {
      const active = tab.dataset.panel === next;
      tab.classList.toggle("selected", active);
      if (active) tab.setAttribute("aria-current", "page"); else tab.removeAttribute("aria-current");
    }
    for (const candidate of CONSOLE_PANELS) {
      const pane = $("#cc-panel-" + candidate.id);
      pane.hidden = candidate.id !== next;
      pane.setAttribute("aria-hidden", String(candidate.id !== next));
    }
    $("#cc-page-title").textContent = page.label;
    $("#cc-page-group").textContent = page.group;
    $("#cc-page-description").textContent = page.description;
    $("#cc-phase").hidden = next !== "talk" && !turnBusy && !recordPurpose && !captureConverting;
    $("#cc-approval-banner").hidden = pendingPermissionCount === 0 || next === "tools";
    $("#cc-question-banner").hidden = pendingQuestionCount === 0 || next === "talk";
    try { localStorage.setItem("espbot.console.panel", next); } catch { /* Navigation remains usable without storage. */ }
    if (["settings", "models", "devices", "tools"].includes(next)) void refreshStatus();
    if (next === "family" || next === "memory") void refreshFamily();
    if (next === "devices") void refreshCredentials().catch((error) => notice(errorText(error), true));
    if (next === "tools") void refreshPermissions();
    householdController.setPanel(next, visible);
  }
  function updateControls(): void {
    const blocked = turnBusy || operationBusy || captureConverting;
    $<HTMLButtonElement>("#cc-send").disabled = blocked || !serviceReady || recordPurpose !== null || (!wavDraft && !input.value.trim());
    input.disabled = turnBusy || wavDraft !== null;
    profileSelect.disabled = blocked || recordPurpose !== null;
    privateScope.disabled = blocked || recordPurpose !== null || !selectedProfile;
    if (!selectedProfile) privateScope.checked = false;
    $<HTMLButtonElement>("#cc-start").disabled = blocked || serviceReady;
    $<HTMLButtonElement>("#cc-service-stop").disabled = operationBusy || !serviceRunning;
    // Device turns may be waiting, presenting or listening after foreground
    // inference has returned. Only the backend's live lifecycle is authoritative;
    // a retained result card and unrelated configuration writes are not work to stop.
    $<HTMLButtonElement>("#cc-stop").disabled = stopBusy || !(turnBusy || recordPurpose !== null
      || captureConverting || cameraDialog.open || audio !== null || householdPlaying || backendCancellable);
    memberFocus.disabled = blocked;
    $<HTMLButtonElement>("#cc-record").disabled = blocked || (recordPurpose !== null && recordPurpose !== "chat");
    $<HTMLButtonElement>("#cc-choose-wav").disabled = blocked || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-record").textContent = recordPurpose === "chat" ? recorder.starting ? "取消打开麦克风" : "■ 结束录音" : "● 录一段";
    $<HTMLButtonElement>("#cc-enroll").disabled = blocked || recordPurpose !== null || !selectedProfile
      || samples.length < minSamples || !$<HTMLInputElement>("#cc-enroll-consent").checked;
    $<HTMLButtonElement>("#cc-sample-camera").disabled = blocked || !selectedProfile || identityKind.value !== "face" || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-sample-record").disabled = blocked || !selectedProfile || identityKind.value !== "speaker"
      || (recordPurpose !== null && recordPurpose !== "enroll");
    $<HTMLButtonElement>("#cc-sample-record").textContent = recordPurpose === "enroll" ? recorder.starting ? "取消打开麦克风" : "■ 结束样本录音" : "录一段样本";
    $<HTMLButtonElement>("#cc-add-samples").disabled = blocked || !selectedProfile || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-clear-samples").disabled = blocked || recordPurpose !== null;
    for (const control of Array.from(container.querySelectorAll<HTMLButtonElement>("#cc-sample-list button"))) control.disabled = blocked || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-recognize-file").disabled = blocked || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-recognize-camera").disabled = blocked || recognizeKind.value !== "face" || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-recognize-record").disabled = blocked || recognizeKind.value !== "speaker"
      || (recordPurpose !== null && recordPurpose !== "recognize");
    $<HTMLButtonElement>("#cc-recognize-record").textContent = recordPurpose === "recognize" ? recorder.starting ? "取消打开麦克风" : "■ 结束并识别" : "录音识别";
    $("#cc-sample-camera").hidden = identityKind.value !== "face";
    $("#cc-sample-record").hidden = identityKind.value !== "speaker";
    $("#cc-recognize-camera").hidden = recognizeKind.value !== "face";
    $("#cc-recognize-record").hidden = recognizeKind.value !== "speaker";
    identityKind.disabled = blocked || recordPurpose !== null;
    recognizeKind.disabled = blocked || recordPurpose !== null;
    const keyConfigurable = credentialRoutes.some((route) => route.id === keyRoute.value && route.credential_editable === true);
    keyRoute.disabled = blocked || credentialRoutes.length === 0;
    keyInput.disabled = blocked || !keyConfigurable;
    $<HTMLButtonElement>("#cc-api-key-save").disabled = blocked || !keyConfigurable || !keyInput.value.trim();
    $<HTMLButtonElement>("#cc-api-key-delete").disabled = blocked || !keyConfigurable
      || !credentialRoutes.some((route) => route.id === keyRoute.value && route.keychain_saved === true);
    $<HTMLButtonElement>("#cc-api-key-save").textContent = credentialRoutes.some((route) => route.id === keyRoute.value && route.keychain_saved === true) ? "更新密钥" : "保存密钥";
    for (const form of Array.from(container.querySelectorAll<HTMLFormElement>("#cc-route-form, #cc-local-form, #cc-tools-form, #cc-native-form, #cc-gateway-form, #cc-preferences-form"))) {
      for (const control of Array.from(form.querySelectorAll<HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement | HTMLButtonElement>("input, textarea, select, button"))) control.disabled = blocked || !loadedConfiguration;
    }
    $("#cc-enrollment-owner").textContent = selectedProfile ? `登记给：${profileName(selectedProfile)}。请确认样本都是本人。` : "先在上方选择成员，再添加登记样本。";
    $("#cc-sample-guidance").textContent = identityKind.value === "face"
      ? `${minSamples}–${maxSamples} 张不同照片，每张只有本人。样本留在本机。`
      : `${minSamples}–${maxSamples} 段不同录音，每段 3–30 秒，仅本人说话。样本留在本机。`;
  }
  async function perform(label: string, action: () => Promise<void>): Promise<void> {
    if (operationBusy) { notice("另一项本地操作还在进行，请稍等。"); return; }
    operationBusy = true; updateControls(); notice(label);
    try { await action(); } catch (error) { notice(errorText(error), true); }
    finally { operationBusy = false; updateControls(); void refreshStatus(); }
  }
  function clearToken(): void {
    $("#cc-token-value").textContent = "";
    $("#cc-token-once").hidden = true;
  }
  function stopPlayback(): void {
    if (audio) { audio.pause(); audio.removeAttribute("src"); audio.load(); audio.remove(); }
    audio = null;
    if (audioUrl) URL.revokeObjectURL(audioUrl);
    audioUrl = null;
    updateControls();
  }
  function clearReplayClips(): void {
    for (const clip of replayClips.values()) { clip.control.disabled = true; clip.control.textContent = "音频已释放"; }
    replayClips.clear();
  }
  function closeCamera(): void { cameraRequestGeneration += 1; camera.close(); if (cameraDialog.open) cameraDialog.close(); updateControls(); }
  function cancelCapture(): void {
    const hadCapture = recordPurpose !== null || captureConverting;
    mediaGeneration += 1;
    recorder.cancel(); recordPurpose = null; captureConverting = false;
    $("#cc-capture-hint").textContent = "按下才录音，最长 30 秒；结束后点“发送”。不用连接机器人。";
    if (hadCapture && !turnBusy) phase("idle", "本地采集已停止。");
    updateControls();
  }
  async function stopConsole(show = true, all = false): Promise<void> {
    if (stopBusy) return;
    stopBusy = true;
    turnGeneration += 1;
    const turn = activeTurn;
    activeTurn = null; turnBusy = false;
    cancelCapture(); closeCamera(); stopPlayback(); householdController.stopMedia(); clearToken();
    wavDraft = null; renderDraft(); phase("idle", "已停止本地采集和播放。");
    updateControls();
    try {
      if (all) await request("assistant.cancel", { scope: "all" });
      else if (turn) await request("assistant.cancel", { scope: "turn", turn_id: turn });
      if (show) notice(all ? "已发送停止互动请求；后台任务可在“任务与作品”单独取消。" : "已停止本轮互动。");
    }
    catch (error) { if (show) notice(`本地采集和播放已停止；后端取消结果：${errorText(error)}`, true); }
    finally { stopBusy = false; updateControls(); void refreshStatus(); }
  }
  function resetConversationView(): void {
    stopPlayback(); clearReplayClips();
    conversationId = newId("console");
    activeTurn = null; turnBusy = false; input.value = "";
    wavDraft = null; renderDraft(); messages.replaceChildren();
    renderWelcome(); updateControls();
  }
  function renderWelcome(): void {
    if (messages.children.length) return;
    const welcome = element("div", "cc-welcome");
    welcome.append(element("span", "cc-welcome-mark", "✦"), element("h2", "", "今天，从一个小问题开始"),
      element("p", "", "文字、声音和屏幕卡片一起用。不连接机器人，也能在这台 Mac 上聊。"));
    const suggestions = element("div", "cc-suggestions");
    for (const text of ["为什么月亮会变形？", "用恐龙讲一道数学题", "陪我安排放学后的时间"]) {
      suggestions.append(button(text, () => { input.value = text; updateControls(); input.focus(); }, "cc-suggestion"));
    }
    welcome.append(suggestions); messages.append(welcome);
  }
  function chatRow(role: "user" | "assistant", text: string): { row: HTMLElement; body: HTMLElement } {
    messages.querySelector(".cc-welcome")?.remove();
    const row = element("article", `cc-message ${role}`);
    const label = element("div", "cc-message-label", role === "assistant" ? "小呆" : selectedProfile ? profileName(selectedProfile) : "你");
    const body = element("div", "cc-message-body", text);
    row.append(label, body); messages.append(row);
    while (messages.children.length > 60) {
      const first = messages.firstElementChild;
      if (first && audio && first.contains(audio)) stopPlayback();
      first?.remove();
    }
    messages.scrollTop = messages.scrollHeight;
    return { row, body };
  }
  function mediaUrl(raw: string | null): string | null {
    if (!raw || raw.length > 2048) return null;
    try {
      const url = new URL(raw);
      return url.protocol === "https:" && !url.username && !url.password && (!url.port || url.port === "443")
        && approvedMediaHosts.has(url.hostname.toLowerCase()) ? url.href : null;
    } catch { return null; }
  }
  function renderCard(card: Card): HTMLElement {
    const article = element("section", "cc-card");
    if (card.title) article.append(element("h3", "", card.title));
    if (card.kind === "steps") {
      const steps = element("ol");
      for (const text of card.body.split(/\n+/).filter((line) => line.trim()).slice(0, 16)) {
        steps.append(element("li", "", text.replace(/^\s*(?:[-*•]|\d+[.)、])\s*/, "")));
      }
      article.append(steps);
    } else if (card.body) article.append(element("p", "", card.body));
    const url = mediaUrl(card.url);
    if (url && card.kind === "image") {
      const image = element("img", "cc-card-image"); image.alt = card.title || "小呆找到的说明图片";
      image.loading = "lazy"; image.decoding = "async"; image.referrerPolicy = "no-referrer"; image.src = url;
      image.addEventListener("error", () => { image.remove(); article.append(element("p", "cc-caption", "这张图片暂时无法加载。")); }, { once: true });
      article.append(image);
    } else if (url && card.kind === "web") {
      const frame = element("iframe", "cc-card-web");
      frame.title = card.title || "说明网页"; frame.setAttribute("sandbox", "");
      frame.allow = ""; frame.referrerPolicy = "no-referrer"; frame.loading = "lazy"; frame.src = url;
      article.append(frame, element("p", "cc-caption", "页面可能拒绝嵌入；上方文字说明仍可阅读。"));
    }
    if (url) article.append(element("span", "cc-source", new URL(url).hostname));
    return article;
  }
  async function playReply(base64: string, host: HTMLElement, generation: number, artifactIds: string[] = []): Promise<void> {
    if (disposed || !visible || generation !== turnGeneration) return;
    stopPlayback();
    const player = element("audio", "cc-reply-audio"); player.controls = true; player.preload = "auto";
    const caption = element("p", "cc-caption"); host.replaceChildren(caption, player);
    audio = player; updateControls();
    const current = () => !disposed && visible && generation === turnGeneration && audio === player;
    let index = 0, advancing = false;
    const count = artifactIds.length || 1;
    const advance = async (): Promise<void> => {
      if (!current() || advancing) return;
      advancing = true;
      try {
        caption.textContent = `正在准备第 ${index + 1} / ${count} 段语音…`;
        let bytes: ArrayBuffer | null;
        if (artifactIds.length) {
          const asset = object(await request("artifact.get", { artifact_id: artifactIds[index] }));
          if (!current()) return;
          if (asset.artifact_id !== artifactIds[index] || asset.mime_type !== "audio/wav") throw new Error("本段语音素材与回复不一致。");
          bytes = await readArtifactContent(request, asset, current, undefined, 24 * 1024 * 1024);
        } else bytes = base64ToBuffer(base64, 24 * 1024 * 1024);
        if (!bytes || !current()) return;
        if (audioUrl) URL.revokeObjectURL(audioUrl);
        audioUrl = URL.createObjectURL(new Blob([bytes], { type: "audio/wav" })); player.src = audioUrl;
        caption.textContent = `第 ${index + 1} / ${count} 段语音`;
        try { await player.play(); }
        catch { if (current()) notice("本段语音已准备好，点击播放器继续播放。"); }
      } catch (error) {
        if (current()) { stopPlayback(); caption.textContent = "语音播放已停止。"; notice(errorText(error), true); }
      } finally { advancing = false; }
    };
    player.addEventListener("playing", () => { if (current()) phase("speaking"); });
    player.addEventListener("error", () => { if (current()) { stopPlayback(); caption.textContent = "当前系统无法播放本段语音。"; notice(caption.textContent, true); } });
    player.addEventListener("ended", () => {
      if (!current()) return;
      if (++index < count) { void advance(); return; }
      stopPlayback(); caption.textContent = "本次回复的语音已播完。";
      if (!turnBusy) phase("idle", "还想接着聊哪一点？", "happy");
    });
    await advance();
  }
  function appendHouseholdLinks(host: HTMLElement, value: unknown): void {
    const result = object(value);
    const source = string(object(result.journal).conversation_turn_id);
    const presentation = string(result.presentation_id, string(object(result.presentation).presentation_id));
    const taskIds = list(result.task_ids).filter((id): id is string => typeof id === "string" && !!id);
    if (!source && !presentation && !taskIds.length) return;
    const actions = element("div", "cc-inline");
    const run = async (target: ConsolePanel, action: () => Promise<void>) => {
      activatePanel(target);
      try { await action(); } catch (error) { notice(errorText(error), true); }
    };
    if (source) actions.append(button("查看本轮来源 / 标注", () => run("household", () => householdController.showSource(source))));
    if (presentation) actions.append(button("查看图文与语音", () => run("tasks", () => householdController.showPresentation(presentation))));
    taskIds.slice(0, 8).forEach((id, index) => actions.append(button(taskIds.length === 1 ? "查看后台任务" : `查看任务 ${index + 1}`, () => run("tasks", () => householdController.showTask(id)))));
    host.append(actions);
    if (taskIds.length) host.append(element("p", "cc-caption", "任务已接下，处理进度和最终作品会保存在“任务与作品”。"));
  }
  async function sendTurn(): Promise<void> {
    if (turnBusy || operationBusy || recordPurpose) return;
    if (!serviceReady) { notice("请先启动小呆服务，再发送这一条。", true); return; }
    const text = input.value.trim();
    const draft = wavDraft;
    if (!draft && !text) return;
    const generation = ++turnGeneration;
    const turnId = newId("turn");
    const currentConversation = conversationId;
    let received = false;
    activeTurn = turnId; turnBusy = true; stopPlayback();
    const user = chatRow("user", draft ? `语音 · ${draft.seconds.toFixed(1)} 秒（正在识别）` : text);
    input.value = ""; wavDraft = null; renderDraft(); updateControls();
    notice(""); phase("thinking", draft ? "先在 Mac 上听懂这句话…" : undefined);
    try {
      const result = await request<TurnResult>("assistant.turn", {
        turn_id: turnId, conversation_id: currentConversation,
        speaker_person_id: selectedProfile, profile_id: privateScope.checked ? selectedProfile : null,
        speak: $<HTMLInputElement>("#cc-speak").checked,
        ...(draft ? { wav_base64: draft.base64 } : { text }),
      });
      if (generation !== turnGeneration || currentConversation !== conversationId || disposed) return;
      received = true;
      if (draft) user.body.textContent = result.transcript;
      const reply = chatRow("assistant", result.scene.speech);
      for (const card of result.scene.cards.slice(0, 4)) reply.row.append(renderCard(card));
      const meta = element("div", "cc-reply-meta", `${result.provider_id} · ${result.model_id}`);
      reply.row.append(meta);
      const timingDetails = renderTurnTimings(result.timings);
      if (timingDetails) reply.row.append(timingDetails);
      appendHouseholdLinks(reply.row, result);
      const wav = string(object(result.audio).wav_base64);
      const audioAssets = list(object(result.presentation).segments).map(segment => string(object(segment).speech_asset_id)).filter(Boolean);
      if (wav || audioAssets.length) {
        const audioHost = element("div", "cc-audio-host");
        const play = button("播放回复", async () => {
          const clip = replayClips.get(turnId);
          if (clip) await playReply(clip.base64, audioHost, turnGeneration, clip.artifactIds);
        });
        replayClips.set(turnId, { base64: wav, artifactIds: audioAssets, control: play });
        while (replayClips.size > 3) {
          const oldest = replayClips.keys().next().value as string | undefined;
          if (!oldest) break;
          const old = replayClips.get(oldest);
          if (old) { old.control.disabled = true; old.control.textContent = "音频已释放"; }
          replayClips.delete(oldest);
        }
        reply.row.append(play, audioHost);
        if ($<HTMLInputElement>("#cc-speak").checked && visible) void playReply(wav, audioHost, generation, audioAssets);
      }
      if (result.identity_candidate) renderIdentity(result.identity_candidate, true);
      if (result.warnings?.length) notice(result.warnings.map(errorText).join("\n"));
      phase("idle", result.task_ids?.length ? "这件事已接下，可以在“任务与作品”查看进度。" : "还想接着聊哪一点？", result.scene.expression);
      messages.scrollTop = messages.scrollHeight;
    } catch (error) {
      if (generation === turnGeneration) { notice(errorText(error), true); phase("error"); }
    } finally {
      if (draft && !received) user.body.textContent = `语音 · ${draft.seconds.toFixed(1)} 秒（${generation === turnGeneration ? "本轮未完成" : "已停止"}）`;
      if (generation === turnGeneration) { activeTurn = null; turnBusy = false; updateControls(); void refreshPermissions(); }
    }
  }
  function renderDraft(): void {
    $("#cc-draft").hidden = !wavDraft;
    $("#cc-draft-label").textContent = wavDraft ? `语音 ${wavDraft.seconds.toFixed(1)} 秒 · 16 kHz 单声道 · 待发送` : "";
    updateControls();
  }
  async function toggleRecording(purpose: CapturePurpose): Promise<void> {
    if (recordPurpose === purpose) { await finishRecording(); return; }
    if (turnBusy || operationBusy || captureConverting || recordPurpose) return;
    if (purpose === "enroll" && !selectedProfile) { notice("请先选择要登记的家庭成员。", true); return; }
    stopPlayback(); closeCamera();
    const generation = ++mediaGeneration;
    recordPurpose = purpose; phase("listening", "正在打开麦克风；再次点击可取消。"); updateControls();
    try {
      const started = recorder.start({
        progress: (seconds) => {
          if (generation !== mediaGeneration) return;
          const text = `正在录音 ${seconds.toFixed(1)} / 30 秒 · 再按一次结束`;
          $("#cc-capture-hint").textContent = text; $("#cc-phase").textContent = text;
        },
        limit: () => { if (generation === mediaGeneration) void finishRecording(); },
      });
      updateControls();
      await started;
      if (generation === mediaGeneration) updateControls();
    } catch (error) {
      if (generation === mediaGeneration) { cancelCapture(); notice(captureErrorText(error), true); phase("idle"); }
    }
  }
  async function finishRecording(): Promise<void> {
    const purpose = recordPurpose;
    if (!purpose || captureConverting) return;
    if (recorder.starting) {
      cancelCapture(); notice("已取消打开麦克风。"); return;
    }
    if (!recorder.recording) { cancelCapture(); return; }
    const generation = mediaGeneration;
    captureConverting = true; updateControls();
    try {
      const wav = await recorder.finish();
      if (generation !== mediaGeneration || disposed) return;
      recordPurpose = null; captureConverting = false; updateControls();
      if (purpose === "chat") {
        wavDraft = wav; renderDraft(); notice("录音已准备好。点“发送”后才开始识别与回答。");
      } else if (purpose === "enroll") {
        if (wav.seconds < 3) throw new Error("声纹样本至少需要 3 秒，请多说一句话。");
        addSample({ base64: wav.base64, label: `录音样本 · ${wav.seconds.toFixed(1)} 秒` });
      } else {
        await perform("正在本地识别声纹…", () => recognize("speaker", wav.base64));
      }
      phase("idle");
    } catch (error) {
      if (generation === mediaGeneration) {
        recorder.cancel();
        notice(errorText(error), true);
      }
    }
    finally {
      if (generation === mediaGeneration) {
        recordPurpose = null; captureConverting = false;
        $("#cc-capture-hint").textContent = wavDraft ? "录音已结束，麦克风已关闭。点击“发送”继续。" : "按下才录音，最长 30 秒；结束后点“发送”。不用连接机器人。";
        if (!turnBusy && !audio) phase("idle");
        updateControls();
      }
    }
  }
  function clearSamples(): void {
    cancelCapture(); closeCamera(); samples = [];
    $<HTMLInputElement>("#cc-enroll-consent").checked = false;
    $<HTMLInputElement>("#cc-enroll-replace").checked = false;
    $<HTMLInputElement>("#cc-samples").value = "";
    renderSamples(); updateControls();
  }
  function addSample(sample: Sample): void {
    if (samples.length >= maxSamples) { notice(`最多保留 ${maxSamples} 份待登记样本。`, true); return; }
    if (samples.some((previous) => previous.base64 === sample.base64)) { notice("这一份样本已经添加，请换一次拍摄或录音。", true); return; }
    samples.push(sample); renderSamples(); updateControls();
  }
  function renderSamples(): void {
    const host = $("#cc-sample-list"); host.replaceChildren();
    samples.forEach((sample, index) => {
      const row = element("div", "cc-sample");
      row.append(element("span", "", `${index + 1}. ${sample.label}`), button("移除", () => { samples.splice(index, 1); renderSamples(); updateControls(); }));
      host.append(row);
    });
    if (!samples.length) host.append(element("span", "cc-caption", "还没有待登记样本。"));
  }
  async function openCamera(purpose: "enroll" | "recognize"): Promise<void> {
    if (turnBusy || operationBusy || recordPurpose) return;
    if (purpose === "enroll" && !selectedProfile) { notice("请先选择要登记的家庭成员。", true); return; }
    cameraPurpose = purpose;
    const generation = ++cameraRequestGeneration;
    $<HTMLButtonElement>("#cc-camera-snapshot").disabled = true;
    $("#cc-camera-title").textContent = purpose === "enroll" ? `给 ${profileName(selectedProfile)} 拍一份人脸样本` : "拍照识别一次";
    if (!cameraDialog.open) cameraDialog.showModal();
    updateControls();
    try {
      await camera.open($<HTMLVideoElement>("#cc-camera-video"));
      if (generation === cameraRequestGeneration && cameraDialog.open) $<HTMLButtonElement>("#cc-camera-snapshot").disabled = false;
    } catch (error) {
      if (generation === cameraRequestGeneration) { closeCamera(); notice(captureErrorText(error), true); }
    }
  }
  async function recognize(kind: IdentityKind, media: string): Promise<void> {
    const operation = ++identityOperation;
    const result = await request("identity.recognize", { kind, ...(kind === "face" ? { image_base64: media } : { wav_base64: media }) });
    if (operation !== identityOperation || disposed) return;
    renderIdentity(result); notice("识别完成；本轮成员没有被自动改变。");
  }
  function renderIdentity(value: unknown, fromConversation = false): void {
    const result = object(value);
    const state = string(result.status, "unknown");
    const names: Record<string, string> = { matched: "找到一个候选", unknown: "暂时不认识", ambiguous: "无法分清候选", low_quality: "样本质量不足", no_enrollment: "还没有该模型的登记" };
    const host = $("#cc-identity-result"); host.replaceChildren();
    host.append(element("strong", "", names[state] ?? "识别未确定"));
    if (state === "matched" && typeof result.profile_id === "string") {
      host.append(element("p", "", `与 ${profileName(result.profile_id)} 的登记样本接近。本轮成员仍由你选择。`));
    }
    if (typeof result.best_score === "number") host.append(element("span", "cc-caption", `余弦相似度 ${result.best_score.toFixed(3)}，不是正确率。`));
    const details = element("details"); details.append(element("summary", "", "识别细节"), element("pre", "cc-json", pretty(result, 6000))); host.append(details);
    if (fromConversation && state === "matched") host.append(element("p", "cc-caption", "来自刚才主动发送的语音。"));
  }

  function renderProfiles(): void {
    const previousOwner = memoryOwner.value;
    profileSelect.replaceChildren(new Option("未指定 · 家庭共同对话", ""));
    memoryOwner.replaceChildren(new Option("请选择成员", ""));
    memberFocus.replaceChildren(new Option("未指定成员", ""));
    const host = $("#cc-profile-list"); host.replaceChildren();
    for (const profile of profiles.filter(profile => !profile.merged_into)) {
      profileSelect.add(new Option(profile.display_name, profile.id));
      memoryOwner.add(new Option(profile.display_name, profile.id));
      memberFocus.add(new Option(profile.display_name, profile.id));
      const row = element("div", "cc-manage-row");
      const meta = element("div", "cc-manage-meta");
      meta.append(element("strong", "", profile.display_name), element("span", "cc-caption", profile.is_child ? "儿童档案 · 手动指定" : "家庭成员"));
      const actions = element("div", "cc-inline");
      actions.append(button(profile.id === selectedProfile ? "已选择" : "选择此人", () => changeProfile(profile.id)),
        button("编辑", () => {
          editingProfileId = profile.id;
          $<HTMLInputElement>("#cc-profile-edit-name").value = profile.display_name;
          $<HTMLInputElement>("#cc-profile-edit-child").checked = profile.is_child;
          profileDialog.showModal();
        }),
        button("删除", () => perform("正在删除档案与关联数据…", async () => {
          if (!window.confirm(`删除“${profile.display_name}”的档案、关联记忆、声纹和人脸登记？对应对话也会清理。`)) { notice(""); return; }
          await stopConsole(false); await request("profile.delete", { profile_id: profile.id });
          if (selectedProfile === profile.id) selectedProfile = null;
          clearSamples(); resetMemoryForm(); resetConversationView(); await refreshFamily();
          notice("档案与关联数据已删除。");
        }), "cc-button danger"));
      row.append(meta, actions); host.append(row);
    }
    profileSelect.value = selectedProfile ?? "";
    memberFocus.value = selectedProfile ?? "";
    memoryOwner.value = editingMemory?.profile_id ?? (previousOwner || selectedProfile || "");
    if (!profiles.some(profile => !profile.merged_into)) host.append(element("p", "cc-muted", "还没有家庭成员档案。可以先匿名聊天。"));
    updateControls();
  }
  async function refreshFamily(): Promise<void> {
    const results = await Promise.allSettled([
      request<Profile[]>("profile.list"), request<MemoryRecord[]>("memory.list"), request<Enrollment[]>("identity.list"),
      request<UnattributedMemory[]>("memory.unattributed.list"),
    ]);
    if (disposed) return;
    const [people, notes, identities, unattributed] = results;
    if (people.status === "fulfilled") {
      profiles = people.value;
      if (selectedProfile && !profiles.some((profile) => profile.id === selectedProfile && !profile.merged_into)) {
        await stopConsole(false); selectedProfile = null; clearSamples(); resetMemoryForm(); resetConversationView();
      }
      renderProfiles();
    }
    if (notes.status === "fulfilled") memories = notes.value;
    if (unattributed.status === "fulfilled") unattributedMemories = unattributed.value;
    renderMemories();
    if (identities.status === "fulfilled") { enrollments = identities.value; renderEnrollments(); }
    for (const result of results) if (result.status === "rejected") { notice(errorText(result.reason), true); break; }
  }
  async function changeProfile(id: string | null): Promise<void> {
    if (id === selectedProfile) return;
    if (id && !profiles.some((profile) => profile.id === id && !profile.merged_into)) return;
    const changedPrivateScope = privateScope.checked;
    await stopConsole(false);
    selectedProfile = id; clearSamples(); resetMemoryForm();
    // A speaker label does not select private memory or split the family thread.
    // Only an explicit private-scope change starts a separate visible exchange.
    if (changedPrivateScope) resetConversationView();
    renderProfiles(); renderEnrollments();
    $("#cc-identity-result").textContent = "本轮成员已改变，尚未重新请求识别。";
    notice(privateScope.checked ? `已切换到 ${profileName(id)} 的个人范围。`
      : id ? `本轮说话人标为 ${profileName(id)}，继续家庭共同对话。` : "本轮不指定说话人，继续家庭共同对话。");
  }
  function renderEnrollments(): void {
    const host = $("#cc-enrollment-list"); host.replaceChildren();
    const relevant = enrollments.filter((enrollment) => enrollment.profile_id === selectedProfile);
    for (const enrollment of relevant) {
      const row = element("div", "cc-manage-row");
      const meta = element("div", "cc-manage-meta");
      meta.append(element("strong", "", `${enrollment.kind === "face" ? "人脸" : "声纹"} · ${enrollment.sample_count} 份样本`));
      const details = element("details"); details.append(element("summary", "cc-caption", "模型版本"), element("code", "cc-small-code", enrollment.model_id)); meta.append(details);
      row.append(meta, button("撤销", () => perform("正在撤销登记…", async () => {
        if (!window.confirm(`撤销 ${profileName(enrollment.profile_id)} 的${enrollment.kind === "face" ? "人脸" : "声纹"}登记？`)) { notice(""); return; }
        await request("identity.revoke", { profile_id: enrollment.profile_id, kind: enrollment.kind });
        $("#cc-identity-result").textContent = "登记已撤销，请重新识别后再查看候选。";
        await refreshFamily(); notice("登记已撤销。");
      }), "cc-button danger"));
      host.append(row);
    }
    if (selectedProfile && !relevant.length) host.append(element("p", "cc-caption", "该成员尚未登记声纹或人脸。"));
  }
  function renderMemories(): void {
    const query = $<HTMLInputElement>("#cc-memory-filter").value.trim().toLowerCase();
    const shown = memories.filter((memory) => `${memory.content} ${memory.category} ${profileName(memory.profile_id)}`.toLowerCase().includes(query));
    const pending = unattributedMemories.filter((item) => `${item.content} ${item.category}`.toLowerCase().includes(query));
    $("#cc-memory-count").textContent = `(${shown.length + pending.length})`;
    const host = $("#cc-memory-list"); host.replaceChildren();
    for (const memory of shown.slice(0, 100)) {
      const card = element("article", "cc-memory-item");
      const tags = element("div", "cc-memory-tags");
      tags.append(element("span", "cc-tag", memory.scope === "household" ? "家庭共享" : profileName(memory.profile_id)),
        element("span", `cc-tag ${memory.confirmed ? "good" : "pending"}`, memory.source_ref?.startsWith("auto:") ? "自动提取" : memory.confirmed ? "已确认" : "待确认"),
        element("span", "cc-tag", memory.allow_cloud ? "允许云端" : "仅本地"));
      card.append(tags, element("p", "cc-memory-text", memory.content));
      const meta = element("div", "cc-memory-footer");
      meta.append(element("span", "cc-caption", `${memory.category}${memory.expires_at_ms ? ` · 到期 ${new Date(memory.expires_at_ms).toLocaleString()}` : ""}`));
      const actions = element("div", "cc-inline");
      actions.append(button("编辑", () => editMemory(memory)), button("删除", () => perform("正在删除记忆…", async () => {
        if (!window.confirm(`删除这条记忆，并清理对话中的旧副本？\n\n${memory.content.slice(0, 200)}`)) { notice(""); return; }
        await stopConsole(false); await request("memory.delete", { id: memory.id });
        resetConversationView(); if (editingMemory?.id === memory.id) resetMemoryForm(); await refreshFamily(); notice("记忆已删除，对话已重新开始。");
      }), "cc-button danger"));
      meta.append(actions); card.append(meta); host.append(card);
    }
    for (const item of pending) {
      const card = element("article", "cc-memory-item");
      const tags = element("div", "cc-memory-tags");
      tags.append(element("span", "cc-tag pending", "身份未确定"), element("span", "cc-tag", "自动提取 · 暂不参与个人召回"));
      card.append(tags, element("p", "cc-memory-text", item.content), element("p", "cc-caption", `原话：${item.quote}`));
      const actions=element("div", "cc-inline");
      const select=element("select", "cc-input"); select.setAttribute("aria-label", "选择这条记忆所属成员");
      const placeholder=element("option", "", "选择成员"); placeholder.value=""; select.append(placeholder);
      for (const profile of profiles.filter(profile => !profile.merged_into)) {const option=element("option", "", profile.display_name); option.value=profile.id; select.append(option);}
      actions.append(select, button("归属给成员", () => perform("正在关联记忆…", async () => {
        if (!select.value) throw new Error("请先选择成员。");
        await request("memory.unattributed.assign", {id:item.id,profile_id:select.value}); await refreshFamily(); notice("记忆已关联到所选成员。");
      })), button("删除", () => perform("正在删除记忆…", async () => {
        await request("memory.unattributed.delete", {id:item.id}); await refreshFamily(); notice("未归属记忆已删除。");
      }), "cc-button danger"));
      card.append(actions);host.append(card);
    }
    if (!shown.length && !pending.length) host.append(element("p", "cc-muted", query ? "没有匹配的记忆。" : "还没有保存的记忆。记录一些有帮助的小偏好就够了。"));
    if (shown.length > 100) host.append(element("p", "cc-caption", "当前显示前 100 条，请使用搜索缩小范围。"));
  }
  function resetMemoryForm(): void {
    editingMemory = null;
    $<HTMLFormElement>("#cc-memory-form").reset();
    $("#cc-memory-form-title").textContent = "记住一件小事";
    $<HTMLButtonElement>("#cc-memory-save").textContent = "保存记忆";
    $<HTMLSelectElement>("#cc-memory-scope").disabled = false;
    $<HTMLSelectElement>("#cc-memory-scope").value = selectedProfile ? "profile" : "household";
    memoryOwner.value = selectedProfile ?? ""; memoryOwner.disabled = !selectedProfile;
  }
  function editMemory(memory: MemoryRecord): void {
    editingMemory = memory;
    $("#cc-memory-form-title").textContent = "修改这一条记忆";
    $<HTMLButtonElement>("#cc-memory-save").textContent = "保存修改";
    const scope = $<HTMLSelectElement>("#cc-memory-scope"); scope.value = memory.scope; scope.disabled = true;
    memoryOwner.value = memory.profile_id ?? ""; memoryOwner.disabled = true;
    $<HTMLTextAreaElement>("#cc-memory-content").value = memory.content;
    $<HTMLInputElement>("#cc-memory-category").value = memory.category;
    $<HTMLSelectElement>("#cc-memory-source").value = memory.source;
    $<HTMLInputElement>("#cc-memory-ref").value = memory.source_ref ?? "";
    $<HTMLInputElement>("#cc-memory-confirmed").checked = memory.confirmed;
    $<HTMLInputElement>("#cc-memory-cloud").checked = memory.allow_cloud;
    $<HTMLInputElement>("#cc-memory-expiry").value = memory.expires_at_ms
      ? new Date(memory.expires_at_ms - new Date(memory.expires_at_ms).getTimezoneOffset() * 60_000).toISOString().slice(0, 16) : "";
    memoryDialog.showModal();
  }
  async function saveMemory(): Promise<void> {
    const scope = $<HTMLSelectElement>("#cc-memory-scope").value;
    const expiry = $<HTMLInputElement>("#cc-memory-expiry").value;
    const payload = {
      id: editingMemory?.id ?? null, scope, profile_id: scope === "profile" ? memoryOwner.value || null : null,
      content: $<HTMLTextAreaElement>("#cc-memory-content").value.trim(),
      category: $<HTMLInputElement>("#cc-memory-category").value.trim(),
      source: $<HTMLSelectElement>("#cc-memory-source").value,
      source_ref: $<HTMLInputElement>("#cc-memory-ref").value.trim() || null,
      confirmed: $<HTMLInputElement>("#cc-memory-confirmed").checked,
      allow_cloud: $<HTMLInputElement>("#cc-memory-cloud").checked,
      expires_at_ms: expiry ? new Date(expiry).getTime() : null,
    };
    if (scope === "profile" && !payload.profile_id) throw new Error("请为这条个人记忆选择成员。");
    if (payload.expires_at_ms !== null && (!Number.isFinite(payload.expires_at_ms) || payload.expires_at_ms <= Date.now())) throw new Error("到期时间应在将来。");
    await stopConsole(false); await request("memory.save", payload);
    memoryDialog.close(); resetMemoryForm(); resetConversationView(); await refreshFamily(); notice("记忆已保存，对话已重新开始以应用新内容。");
  }
  function renderSpeakerControls(status: JsonObject): void {
    const select = $<HTMLSelectElement>("#cc-speaker-device");
    const slider = $<HTMLInputElement>("#cc-speaker-volume");
    const apply = $<HTMLButtonElement>("#cc-speaker-apply");
    const mute = $<HTMLButtonElement>("#cc-speaker-mute");
    const previous = select.value;
    speakerStates = list(status.device_volumes).map(object).filter((state) => state.online === true && string(state.device_id));
    select.replaceChildren();
    for (const state of speakerStates) {
      const option = element("option", "", string(state.device_id));
      option.value = string(state.device_id); select.append(option);
    }
    if (!speakerStates.length) select.append(element("option", "", "等待设备连接"));
    if (speakerStates.some((state) => state.device_id === previous)) select.value = previous;
    if (select.value !== previous) speakerDirty = false;
    const selected = speakerStates.find((state) => state.device_id === select.value);
    const changedDevice = string(selected?.device_id) !== speakerSelectedId;
    speakerSelectedId = string(selected?.device_id);
    select.disabled = speakerBusy || speakerStates.length === 0;
    const disabled = !selected || speakerBusy || selected.pending === true || selected.settings_open === true;
    slider.disabled = apply.disabled = mute.disabled = disabled;
    if (!selected) {
      $("#cc-speaker-value").textContent = "—";
      $("#cc-speaker-status").textContent = "等待支持音量控制的设备连接。";
      mute.textContent = "静音";
      return;
    }
    if (speakerDirty && !speakerBusy && selected.pending !== true && selected.result === "applied"
      && Number(slider.value) === number(selected.volume)) speakerDirty = false;
    if (!speakerDirty && (selected.pending !== true || changedDevice)) slider.value = String(number(selected.volume));
    $("#cc-speaker-value").textContent = `${slider.value} / 60`;
    mute.textContent = selected.muted === true ? "取消静音" : "静音";
    const current = `当前 ${number(selected.volume)} / 60 · ${selected.muted === true ? "已静音" : "未静音"}`;
    $("#cc-speaker-status").textContent = selected.pending === true ? "正在调整，等待设备确认…"
      : selected.settings_open === true ? "请先关闭小呆上的设置页面。"
      : selected.result === "rejected" ? `${current} · 设备未接受调整。`
      : selected.result === "timed_out" ? `${current} · 等待设备确认超时，请重试。`
      : selected.hardware_applied !== true ? `${current} · 正在等待音频设备应用设置。`
      : `${current}${selected.result === "applied" ? " · 已应用" : ""}`;
  }
  async function setSpeaker(change: { volume?: number; muted?: boolean }): Promise<void> {
    const id = $<HTMLSelectElement>("#cc-speaker-device").value;
    if (!id || speakerBusy) return;
    speakerBusy = true; renderSpeakerControls({ device_volumes: speakerStates });
    try {
      await request("device.volume.set", { device_id: id, ...change });
      await refreshStatus();
    } catch (error) { notice(errorText(error), true); }
    finally { speakerBusy = false; renderSpeakerControls({ device_volumes: speakerStates }); }
  }
  async function refreshStatus(): Promise<void> {
    if (disposed) return;
    if (statusBusy) { statusAgain = true; return; }
    statusBusy = true;
    try {
      const value = object(await request("service.status"));
      if (disposed) return;
      const harness = object(value.harness);
      backendCancellable = list(value.device_activity).some((activity) => object(activity).cancellable === true)
        || list(value.active_turns).length > 0;
      serviceReady = value.ready === true && value.accepting_turns === true && harness.healthy === true;
      const gatewayListening = object(value.gateway).running === true;
      serviceRunning = harness.running === true || gatewayListening;
      const pill = $("#cc-service-pill");
      pill.textContent = serviceReady ? "对话服务运行中" : value.ready === false ? "配置待处理" : gatewayListening ? "等待对话引擎，设备可连接" : "对话服务未启动";
      pill.classList.toggle("ready", serviceReady);
      $("#cc-service-dot").classList.toggle("ready", serviceReady);
      $("#cc-service-dot").classList.toggle("error", value.ready === false || (serviceRunning && !serviceReady));
      $("#cc-service-caption").textContent = `OpenCode ${string(harness.version, string(harness.source_version, "版本待读取"))} · Mac 本机`;
      $("#cc-updated-at").textContent = `更新于 ${new Date().toLocaleTimeString("zh-CN", { hour12: false })} · 每 15 秒刷新`;
      $("#cc-status-summary").textContent = value.ready === false ? string(value.error, "配置未完成")
        : `${serviceReady ? "可以开始对话" : gatewayListening ? "设备入口已监听；可点击启动服务重试对话引擎" : serviceRunning ? "对话服务未就绪，可停止后重试" : "对话服务未启动，可在左下角启动"} · OpenCode ${string(harness.version, string(harness.source_version, "版本待读取"))} · ${string(harness.mode, "managed")}`;
      $("#cc-status-json").textContent = pretty(value);
      $("#cc-config-path").textContent = string(value.config_path);
      const memoryState=object(value.memory), pending=number(memoryState.background_pending), failed=number(memoryState.background_failed);
      $("#cc-memory-background").textContent = value.background_memory_model_ready === true
        ? `本地后台整理 · 待处理 ${pending} 条${failed ? ` · ${failed} 条需要检查` : ""}。身份不明确的内容先暂存。`
        : `本地后台整理模型尚未就绪 · 待处理 ${pending} 条。`;
      renderKeySettings(list(harness.routes));
      renderRoutes(list(harness.routes));
      renderDeviceStatus(container, value);
      renderSpeakerControls(value);
      renderRuntimeMetrics(container, value);
      const mcp = $("#cc-mcp"); mcp.replaceChildren();
      for (const item of list(harness.mcp)) {
        const server = object(item);
        const row = element("div", "cc-manage-row");
        row.append(element("strong", "", string(server.name)), element("span", "cc-caption", server.configured === true ? string(server.status, "已继承，状态待定") : "没有找到现有配置")); mcp.append(row);
      }
      if (!mcp.children.length) mcp.append(element("p", "cc-muted", "启动后读取 browser / deepseek-search 的继承状态。"));
      if (harness.mcp_error) mcp.append(element("p", "cc-caption", string(harness.mcp_error)));
      updateControls();
    } catch (error) {
      if (!disposed) { serviceReady = false; $("#cc-service-pill").textContent = "状态不可用"; $("#cc-service-dot").classList.remove("ready"); $("#cc-service-dot").classList.add("error"); $("#cc-status-summary").textContent = errorText(error); updateControls(); }
    } finally {
      statusBusy = false;
      if (statusAgain && !disposed) { statusAgain = false; void refreshStatus(); }
    }
  }
  function renderRoutes(routes: unknown[]): void {
    const host = $("#cc-routes"); host.replaceChildren();
    routes.forEach((value, index) => {
      const route = object(value);
      const card = element("article", "cc-route");
      card.classList.toggle("selected", route.id === keyRoute.value);
      card.append(element("span", "cc-route-order", String(index + 1)), element("strong", "", string(route.label, string(route.id))), element("code", "", string(route.model_id)));
      let state = route.enabled === false ? "已停用" : route.inherited_provider === true ? "使用 OpenCode 现有登录"
        : route.credential_present !== true ? "等待配置密钥" : "已配置密钥 · 等待实际调用确认";
      if (route.cooling_down === true) state = `暂时跳过 · ${new Date(number(route.retry_at_ms)).toLocaleTimeString()} 后再尝试`;
      card.append(element("span", "cc-caption", state));
      if (route.last_failure) {
        const reason=string(route.last_failure);
        const labels:Record<string,string>={headers_timeout:"服务商响应超时",authentication:"密钥或订阅验证失败",configuration:"模型或端点配置有误",quota_exhausted:"额度不足",rate_limited:"请求暂时受限"};
        card.append(element("span", "cc-caption", labels[reason] ?? reason));
      }
      card.append(route.id === keyRoute.value ? element("span", "cc-tag good", "当前选择") : button("配置此路线", () => selectRoute(string(route.id))));
      if (route.cooling_down === true) card.append(button("重新尝试此路线", () => perform("正在清除该路线的冷却状态…", async () => {
        await request("harness.route.reset", { route_id: string(route.id) }); await refreshStatus(); notice("冷却状态已清除，下次对话按模型顺序重试。");
      })));
      host.append(card);
    });
  }
  function renderKeySettings(routes: unknown[]): void {
    credentialRoutes = routes.map(object);
    const selected = keyRoute.value;
    const editable = credentialRoutes;
    const signature = JSON.stringify(editable.map((route) => [route.id, route.label]));
    // Status refresh must preserve both the selection and an unsaved password.
    if (keyRoute.dataset.signature !== signature) {
      keyRoute.replaceChildren();
      for (const route of editable) {
        const option = element("option", "", string(route.label, string(route.id)));
        option.value = string(route.id); keyRoute.append(option);
      }
      if (editable.some((route) => route.id === selected)) keyRoute.value = selected;
      else keyInput.value = "";
      keyRoute.dataset.signature = signature;
    }
    const route = credentialRoutes.find((item) => item.id === keyRoute.value);
    $("#cc-api-key-status").textContent = !route
      ? "没有可在此配置的路线。Mac 管理模式可保存密钥；attach 模式或继承的 provider 请在 OpenCode 中配置。"
      : route.inherited_provider === true ? "此路线使用 OpenCode 已有的登录。密钥由 OpenCode 管理。"
      : route.credential_error ? `钥匙串状态不可用：${string(route.credential_error)}`
      : `${string(route.label)}：${route.keychain_saved === true ? "已配置 · macOS 钥匙串" : route.credential_env_present === true ? "已配置 · 启动环境变量" : "尚未配置密钥"}。${route.keychain_saved === true && route.credential_env_present === true ? "钥匙串优先；移除后仍会使用环境变量。" : ""}`;
    populateRouteForm();
    updateControls();
  }
  async function changeApiKey(remove = false): Promise<void> {
    const routeId = keyRoute.value;
    const route = credentialRoutes.find((item) => item.id === routeId);
    if (!route || route.credential_editable !== true) return;
    if (remove && !window.confirm(`移除 ${string(route.label)} 保存在本机钥匙串的密钥？服务将停止；已有环境变量和 OpenCode 登录不受影响。`)) return;
    let key = keyInput.value.trim();
    keyInput.value = "";
    if (!remove && (!key || key.length > 4096 || /[^\x21-\x7e]/.test(key))) {
      key = ""; updateControls(); notice("API Key 应为不含空格和换行的英文、数字或符号。", true); return;
    }
    await perform(remove ? "正在移除密钥…" : "正在保存到 macOS 钥匙串…", async () => {
      await stopConsole(false);
      try {
        const pending = request(remove ? "api_key.delete" : "api_key.save", { route_id: routeId, ...(remove ? {} : { api_key: key }) });
        key = "";
        await pending;
        serviceReady = false; updateControls();
        await refreshStatus();
        addActivity(remove ? "已移除本机模型密钥" : "模型密钥已更新");
        notice(remove ? "已移除本机保存的密钥。点击“启动服务”继续。" : "密钥已保存在 macOS 钥匙串。点击“启动服务”生效。");
      } finally { key = ""; }
    });
    key = "";
  }
  async function loadConfiguration(): Promise<void> {
    const config = object(await request("config.read"));
    if (disposed) return;
    loadedConfiguration = config;
    if (!rawConfigDirty) $<HTMLTextAreaElement>("#cc-config-editor").value = pretty(config, 200_000);
    populateConfigurationForms(config);
    approvedMediaHosts = new Set(list(object(config.conversation).media_hosts).map((value) => string(value).toLowerCase()).filter(Boolean));
    $<HTMLInputElement>("#cc-speak").checked = object(config.conversation).default_speak !== false;
    minSamples = Math.min(20, Math.max(3, number(object(config.memory).min_enrollment_samples, 3)));
    maxSamples = Math.min(32, Math.max(minSamples, number(object(config.memory).max_enrollment_samples, 12)));
    updateControls();
  }
  function configuredRoutes(): JsonObject[] {
    return list(object(loadedConfiguration?.harness).routes).map(object);
  }
  function selectRoute(id: string): void {
    if (id !== editingRouteId && routeFormDirty && !window.confirm("所选模型有尚未保存的修改。放弃这些修改并切换路线？")) {
      keyRoute.value = editingRouteId; return;
    }
    keyInput.value = ""; keyRoute.value = id;
    if (editingRouteId !== id) { editingRouteId = ""; routeFormDirty = false; dirtyConfigurationForms.delete("cc-route-form"); }
    renderKeySettings(credentialRoutes); renderRoutes(credentialRoutes);
  }
  function populateRouteForm(force = false): void {
    const routes = configuredRoutes();
    const index = routes.findIndex((route) => route.id === keyRoute.value);
    if (index < 0 || (!force && editingRouteId === keyRoute.value)) return;
    const route = routes[index]; editingRouteId = string(route.id); routeFormDirty = false;
    $<HTMLInputElement>("#cc-route-model").value = string(route.model_id);
    $<HTMLInputElement>("#cc-route-endpoint").value = string(route.base_url);
    $<HTMLInputElement>("#cc-route-enabled").checked = route.enabled !== false;
    const priority = $<HTMLSelectElement>("#cc-route-priority"); priority.replaceChildren();
    routes.forEach((_, i) => priority.add(new Option(`第 ${i + 1} 顺位`, String(i)))); priority.value = String(index);
    $("#cc-route-reference").textContent = `Provider：${string(route.provider_id)} · 密钥引用：${string(route.api_key_env, "使用 OpenCode 登录")}。端点留空则继承 OpenCode provider。`;
    $("#cc-route-dirty").textContent = "";
  }
  function populateConfigurationForms(config: JsonObject): void {
    const harness = object(config.harness), models = object(config.models), gateway = object(config.gateway), conversation = object(config.conversation);
    const canPopulate = (selector: string): boolean => {
      const form = $(selector).closest("form");
      return !form || !dirtyConfigurationForms.has(form.id);
    };
    const inputValue = (selector: string, value: unknown): void => {
      if (canPopulate(selector)) $<HTMLInputElement>(selector).value = typeof value === "number" ? String(value) : string(value);
    };
    const checkValue = (selector: string, checked: boolean): void => {
      if (canPopulate(selector)) $<HTMLInputElement>(selector).checked = checked;
    };
    inputValue("#cc-local-python", models.python); inputValue("#cc-local-script", models.script);
    inputValue("#cc-local-model-dir", models.model_dir); inputValue("#cc-local-voice", models.voice); inputValue("#cc-local-timeout", models.request_timeout_seconds);
    inputValue("#cc-asr-backend", string(models.asr_backend, "whisper"));
    inputValue("#cc-rma-qwen-python", models.rma_qwen_python); inputValue("#cc-rma-qwen-worker", models.rma_qwen_worker); inputValue("#cc-rma-qwen-model", models.rma_qwen_model);
    inputValue("#cc-tools-inherit", harness.inherit_config);
    inputValue("#cc-mcp-names", list(harness.required_mcp).map((value) => string(value)).filter(Boolean).join("\n"));
    const native = new Set(list(harness.native_actions).map((value) => string(value)));
    for (const action of ["read", "glob", "grep", "edit", "shell", "webfetch", "skill"]) {
      checkValue(`#cc-native-${action}`, native.has(action));
    }
    checkValue("#cc-native-external", harness.allow_external_directories === true);
    inputValue("#cc-native-mode", string(harness.native_execution_mode, "ask"));
    inputValue("#cc-gateway-bind", gateway.bind); inputValue("#cc-gateway-max", gateway.max_connections);
    inputValue("#cc-gateway-cert", gateway.certificate_pem); inputValue("#cc-gateway-key", gateway.private_key_pem);
    checkValue("#cc-gateway-enabled", gateway.enabled === true);
    inputValue("#cc-opencode-path", harness.executable); inputValue("#cc-opencode-mode", string(harness.mode, "managed"));
    checkValue("#cc-auto-start", config.auto_start === true);
    checkValue("#cc-default-speak", conversation.default_speak !== false);
    inputValue("#cc-reply-limit", conversation.max_reply_characters); inputValue("#cc-session-idle", conversation.idle_session_seconds);
    inputValue("#cc-media-hosts", list(conversation.media_hosts).map((host) => string(host)).join("\n"));
    if (!dirtyConfigurationForms.has("cc-route-form")) populateRouteForm(true);
  }
  async function patchConfiguration(update: (config: JsonObject) => void, sourceForm: string): Promise<void> {
    if (rawConfigDirty) {
      activatePanel("settings");
      throw new Error("高级配置有尚未保存的内容。请先保存，或点“重新读取”放弃修改，再使用其他设置表单。");
    }
    // Read fresh data and change only this form's fields. Other routes and private paths survive.
    const config = object(await request("config.read"));
    update(config);
    await request("config.save", config);
    if (sourceForm === "cc-route-form") routeFormDirty = false;
    dirtyConfigurationForms.delete(sourceForm);
    await loadConfiguration();
    addActivity("服务设置已更新");
    notice("设置已保存。点击“重载配置”后生效，重载会结束当前设备互动。");
  }
  function formInput(selector: string): string { return $<HTMLInputElement>(selector).value.trim(); }
  function formNumber(selector: string, minimum: number, maximum: number): number {
    const value = Number(formInput(selector));
    if (!Number.isInteger(value) || value < minimum || value > maximum) throw new Error(`数值应为 ${minimum} 到 ${maximum} 之间的整数。`);
    return value;
  }
  function optionalPath(selector: string): string | null {
    const path = formInput(selector);
    if (path && !path.startsWith("/")) throw new Error("文件与目录请填写这台 Mac 的绝对路径，以 / 开头。");
    return path || null;
  }
  function addActivity(label: string): void {
    activity.push({ label, at: Date.now() });
    if (activity.length > 40) activity.shift();
    renderActivity(container, activity);
  }
  function discardConfigurationDrafts(): boolean {
    if ((rawConfigDirty || dirtyConfigurationForms.size > 0) && !window.confirm("重新读取已保存的设置，并放弃各页尚未保存的表单修改？")) return false;
    rawConfigDirty = false; routeFormDirty = false; dirtyConfigurationForms.clear();
    $("#cc-config-dirty").textContent = ""; $("#cc-route-dirty").textContent = "";
    return true;
  }
  async function refreshPermissions(): Promise<void> {
    if (permissionBusy || disposed) return;
    permissionBusy = true;
    try {
      const permissions = list(await request("harness.permissions")).map(object);
      if (disposed) return;
      pendingPermissionCount = permissions.length;
      const host = $("#cc-permissions"); host.hidden = permissions.length === 0;
      $("#cc-permissions-empty").hidden = permissions.length > 0;
      $("#cc-permission-count").textContent = `${permissions.length} 项`;
      $("#cc-nav-pending").hidden = permissions.length === 0;
      $("#cc-nav-pending").textContent = String(permissions.length);
      $("#cc-approval-banner").hidden = permissions.length === 0 || panel === "tools";
      $("#cc-approval-summary").textContent = `${permissions.length} 项工具操作等待确认，任务正在等待你的决定。`;
      livePermissions.clear();
      for (const permission of permissions) {
        livePermissions.set(JSON.stringify([permission.session_id, permission.request_id]), JSON.stringify(permission));
      }
      if (!permissions.length) { host.replaceChildren(); permissionCards.clear(); }
      for (const [key, entry] of permissionCards) {
        if (livePermissions.has(key)) continue;
        // Retain expired rows until the whole batch is done. A remaining
        // approval must never move under a pointer held over another request.
        for (const control of Array.from(entry.card.querySelectorAll<HTMLButtonElement>("button"))) control.disabled = true;
        entry.card.dataset.expired = "true";
        const heading = entry.card.querySelector("strong");
        if (heading) heading.textContent = "这项请求已结束";
      }
      for (const permission of permissions.slice(0, 16)) {
        const key = JSON.stringify([permission.session_id, permission.request_id]);
        const signature = JSON.stringify(permission);
        const previous = permissionCards.get(key);
        if (previous?.signature === signature) continue;
        const name = string(permission.action, string(permission.permission, "工具操作"));
        const details = { action: name, description: permission.description, targets: permission.patterns, actual_details: permission.metadata, model_input: permission.input };
        const encoded = JSON.stringify(details, null, 2) ?? "{}";
        const card = previous?.card ?? element("section", "cc-permission");
        card.replaceChildren(); card.dataset.requestId = string(permission.request_id);
        delete card.dataset.expired;
        permissionCards.set(key, { card, signature });
        card.append(element("strong", "", `请家长确认：${name}`), element("pre", "cc-json", encoded.length <= 24_000 ? encoded : `${encoded.slice(0, 24_000)}\n参数过长，不能在此批准。`));
        const actions = element("div", "cc-inline");
        const reply = async (allow: boolean) => {
          if (permissionReplies.has(key)) return;
          if (livePermissions.get(key) !== signature) { notice("这项请求已结束或改变，请查看当前卡片。", true); return; }
          if (allow && !window.confirm(`仅允许这一次具体操作？\n\n${pretty(details, 4000)}\n\n请求：${string(permission.request_id)}`)) return;
          if (livePermissions.get(key) !== signature) return;
          permissionReplies.add(key);
          for (const control of Array.from(actions.querySelectorAll<HTMLButtonElement>("button"))) control.disabled = true;
          try {
            await request("harness.permission.reply", { session_id: string(permission.session_id), request_id: string(permission.request_id), allow });
            livePermissions.delete(key);
            notice(allow ? "已仅允许这次操作。" : "已拒绝这次操作。");
          } catch (error) { notice(errorText(error), true); }
          finally {
            permissionReplies.delete(key);
            if (livePermissions.get(key) === signature) {
              for (const control of Array.from(actions.querySelectorAll<HTMLButtonElement>("button"))) control.disabled = false;
              allowButton.disabled = encoded.length > 24_000;
            }
            void refreshPermissions();
          }
        };
        const allowButton = button("仅允许这一次", () => reply(true), "cc-button primary");
        allowButton.disabled = encoded.length > 24_000;
        actions.append(allowButton, button("拒绝", () => reply(false), "cc-button danger")); card.append(actions);
        if (!previous) host.append(card);
      }
    } catch (error) { if (turnBusy) notice(errorText(error), true); }
    finally { permissionBusy = false; }
  }
  async function refreshQuestions(): Promise<void> {
    if (questionBusy || disposed) return;
    questionBusy = true;
    try {
      const questions = list(await request("harness.questions")).map(object);
      if (disposed) return;
      pendingQuestionCount = questions.length;
      $("#cc-question-banner").hidden = questions.length === 0 || panel === "talk";
      const host = $("#cc-questions"); host.hidden = questions.length === 0;
      const signature = JSON.stringify(questions);
      if (signature === questionSignature) return; // Keep partially typed answers while polling.
      questionSignature = signature; host.replaceChildren();
      for (const question of questions) {
        const form = element("form", "cc-question");
        form.append(element("strong", "", "小呆想请你确认一件事"));
        const fields = list(question.fields).map(object);
        for (const field of fields) {
          const key = string(field.key), multiple = field.type === "multiselect";
          const group = element("fieldset");
          group.append(element("legend", "", string(field.description, string(field.title, "请选择"))));
          for (const option of list(field.options).map(object)) {
            const label = element("label", "cc-check");
            const choice = element("input"); choice.type = multiple ? "checkbox" : "radio";
            choice.name = key; choice.value = string(option.value);
            const description = string(option.description);
            label.append(choice, element("span", "", `${string(option.label)}${description ? ` — ${description}` : ""}`)); group.append(label);
          }
          const custom = element("textarea"); custom.name = `${key}-custom`; custom.rows = 2; custom.maxLength = 4000;
          custom.placeholder = multiple ? "也可以补充其他选项" : "也可以输入自己的回答"; group.append(custom); form.append(group);
        }
        const actions = element("div", "cc-inline");
        const submit = element("button", "cc-button primary", "发送回答"); submit.type = "submit";
        const skip = button("暂不回答", async () => {
          skip.disabled = true;
          try { await request("harness.question.reply", { session_id: question.session_id, form_id: question.form_id, answer: null }); questionSignature = ""; await refreshQuestions(); }
          catch (error) { notice(errorText(error), true); skip.disabled = false; }
        });
        actions.append(submit, skip); form.append(actions);
        form.addEventListener("submit", (event) => {
          event.preventDefault();
          const data = new FormData(form), answer: JsonObject = {};
          for (const field of fields) {
            const key = string(field.key), custom = string(data.get(`${key}-custom`)).trim();
            const selected = data.getAll(key).filter((value): value is string => typeof value === "string");
            if (field.type === "multiselect") answer[key] = custom ? [...selected, custom] : selected;
            else if (custom || selected.length) answer[key] = custom || selected[0];
          }
          submit.disabled = true; skip.disabled = true;
          void request("harness.question.reply", { session_id: question.session_id, form_id: question.form_id, answer })
            .then(() => { questionSignature = ""; return refreshQuestions(); })
            .catch((error) => { notice(errorText(error), true); submit.disabled = false; skip.disabled = false; });
        });
        host.append(form);
      }
    } catch (error) { if (turnBusy) notice(errorText(error), true); }
    finally { questionBusy = false; }
  }
  async function refreshCredentials(): Promise<void> {
    const credentials = list(await request("device.credentials.list"));
    const host = $("#cc-credential-list"); host.replaceChildren();
    for (const item of credentials) {
      const credential = object(item); const id = string(credential.device_id);
      const row = element("div", "cc-manage-row");
      row.append(element("code", "cc-small-code", id), element("span", "cc-caption", new Date(number(credential.created_at_ms)).toLocaleString()),
        button("撤销", () => perform("正在撤销设备凭据…", async () => {
          if (!window.confirm(`撤销设备 ${id} 的安全连接凭据？`)) { notice(""); return; }
          await request("device.credentials.revoke", { device_id: id }); clearToken(); await refreshCredentials(); notice("设备凭据已撤销。");
        }), "cc-button danger")); host.append(row);
    }
    if (!credentials.length) host.append(element("p", "cc-caption", "尚未签发安全连接凭据。"));
  }

  const on = (selector: string, action: () => void | Promise<void>) => {
    $(selector).addEventListener("click", () => { void action(); });
  };
  $<HTMLSelectElement>("#cc-speaker-device").addEventListener("change", () => {
    speakerDirty = false; renderSpeakerControls({ device_volumes: speakerStates });
  });
  $<HTMLInputElement>("#cc-speaker-volume").addEventListener("input", (event) => {
    speakerDirty = true; $("#cc-speaker-value").textContent = `${(event.target as HTMLInputElement).value} / 60`;
  });
  on("#cc-speaker-apply", () => setSpeaker({ volume: Number($<HTMLInputElement>("#cc-speaker-volume").value) }));
  on("#cc-speaker-mute", () => {
    const selected = speakerStates.find((state) => state.device_id === $<HTMLSelectElement>("#cc-speaker-device").value);
    if (selected) return setSpeaker({ muted: selected.muted !== true });
  });
  const form = (selector: string, label: string, action: () => Promise<void>) => {
    $<HTMLFormElement>(selector).addEventListener("submit", (event) => { event.preventDefault(); void perform(label, action); });
  };
  container.addEventListener("invalid", (event) => {
    // Keep validation actionable when the field lives in a collapsed advanced section.
    let section = (event.target as HTMLElement).closest("details");
    while (section) { section.open = true; section = section.parentElement?.closest("details") ?? null; }
  }, true);
  on("#cc-notice-dismiss", () => notice(""));
  on("#cc-review-permissions", () => activatePanel("tools"));
  on("#cc-review-questions", () => activatePanel("talk"));
  on("#cc-tools-refresh", async () => { await refreshStatus(); await refreshPermissions(); });
  on("#cc-memory-add", () => { resetMemoryForm(); memoryDialog.showModal(); });
  on("#cc-memory-cancel", () => { memoryDialog.close(); resetMemoryForm(); });
  on("#cc-profile-edit-cancel", () => { profileDialog.close(); editingProfileId = null; });
  memberFocus.addEventListener("change", () => { void changeProfile(memberFocus.value || null); });
  form("#cc-profile-edit-form", "正在更新成员档案…", async () => {
    if (!editingProfileId) return;
    const profile = { display_name: formInput("#cc-profile-edit-name"), is_child: $<HTMLInputElement>("#cc-profile-edit-child").checked };
    await stopConsole(false); await request("profile.update", { profile_id: editingProfileId, profile });
    profileDialog.close(); editingProfileId = null; resetConversationView(); await refreshFamily(); notice("成员档案已更新。");
  });
  form("#cc-route-form", "正在保存模型设置…", async () => {
    const routeId = editingRouteId;
    const model = formInput("#cc-route-model"), endpoint = formInput("#cc-route-endpoint");
    const enabled = $<HTMLInputElement>("#cc-route-enabled").checked;
    const priority = formNumber("#cc-route-priority", 0, Math.max(0, configuredRoutes().length - 1));
    if (!model || !routeId) throw new Error("请选择模型路线并填写模型名称。");
    await patchConfiguration((config) => {
      const harness = object(config.harness), routes = list(harness.routes).map(object);
      const index = routes.findIndex((route) => route.id === routeId);
      if (index < 0) throw new Error("这条模型路线已经改变，请重新读取配置。");
      const route = routes.splice(index, 1)[0]; route.model_id = model; route.base_url = endpoint || null; route.enabled = enabled;
      routes.splice(priority, 0, route); harness.routes = routes; config.harness = harness;
    }, "cc-route-form");
  });
  form("#cc-local-form", "正在保存本地模型环境…", async () => {
    const update = { python: formInput("#cc-local-python"), script: optionalPath("#cc-local-script"), model_dir: optionalPath("#cc-local-model-dir"), voice: formInput("#cc-local-voice") || null, request_timeout_seconds: formNumber("#cc-local-timeout", 1, 600), asr_backend: formInput("#cc-asr-backend"), rma_qwen_python: optionalPath("#cc-rma-qwen-python"), rma_qwen_worker: optionalPath("#cc-rma-qwen-worker"), rma_qwen_model: optionalPath("#cc-rma-qwen-model") };
    if (update.asr_backend === "rma_qwen" && (!update.rma_qwen_python || !update.rma_qwen_worker || !update.rma_qwen_model)) throw new Error("使用 RMA Qwen 时，请填写 RMA Python、Worker 和模型目录。");
    await patchConfiguration((config) => { config.models = { ...object(config.models), ...update }; }, "cc-local-form");
    latestModelHealth = null; renderCapabilities(container); $("#cc-models-json").textContent = "本地环境已改变，请重新检查能力状态。";
  });
  form("#cc-tools-form", "正在保存工具配置来源…", async () => {
    const inherited = optionalPath("#cc-tools-inherit");
    const names = [...new Set($<HTMLTextAreaElement>("#cc-mcp-names").value.split(/[\n,]+/).map((name) => name.trim()).filter(Boolean))];
    if (names.length > 16 || names.some((name) => !/^[A-Za-z0-9_-]{1,128}$/.test(name))) throw new Error("最多填写 16 个服务名；名称只能包含英文字母、数字、下划线或连字符。");
    await patchConfiguration((config) => {
      const harness = object(config.harness);
      const readTools = list(harness.read_tools).map((name) => string(name)).filter((name) => names.some((server) => name.startsWith(`${server}_`)));
      config.harness = { ...harness, inherit_config: inherited, required_mcp: names, read_tools: readTools };
    }, "cc-tools-form");
  });
  form("#cc-native-form", "正在保存 OpenCode 工作能力…", async () => {
    const native = ["read", "glob", "grep", "edit", "shell", "webfetch", "skill"]
      .filter((action) => $<HTMLInputElement>(`#cc-native-${action}`).checked);
    const external = $<HTMLInputElement>("#cc-native-external").checked;
    const mode = $<HTMLSelectElement>("#cc-native-mode").value;
    if (native.includes("shell") && !external) throw new Error("终端拥有当前 Mac 账户的权限。要启用终端，请先允许访问工作区外的目录。");
    await patchConfiguration((config) => { config.harness = { ...object(config.harness), native_actions: native, native_execution_mode: mode, allow_external_directories: external }; }, "cc-native-form");
  });
  form("#cc-gateway-form", "正在保存设备网关设置…", async () => {
    const update = { enabled: $<HTMLInputElement>("#cc-gateway-enabled").checked, bind: formInput("#cc-gateway-bind"), max_connections: formNumber("#cc-gateway-max", 1, 64), certificate_pem: optionalPath("#cc-gateway-cert"), private_key_pem: optionalPath("#cc-gateway-key") };
    await patchConfiguration((config) => { config.gateway = { ...object(config.gateway), ...update }; }, "cc-gateway-form");
  });
  form("#cc-preferences-form", "正在保存启动与对话偏好…", async () => {
    const executable = formInput("#cc-opencode-path"), mode = $<HTMLSelectElement>("#cc-opencode-mode").value;
    const autoStart = $<HTMLInputElement>("#cc-auto-start").checked;
    const conversation = { default_speak: $<HTMLInputElement>("#cc-default-speak").checked, max_reply_characters: formNumber("#cc-reply-limit", 100, 4000), idle_session_seconds: formNumber("#cc-session-idle", 30, 1800), media_hosts: [...new Set($<HTMLTextAreaElement>("#cc-media-hosts").value.split(/[\n,]+/).map((host) => host.trim().toLowerCase()).filter(Boolean))] };
    await patchConfiguration((config) => { config.auto_start = autoStart; config.harness = { ...object(config.harness), executable, mode }; config.conversation = { ...object(config.conversation), ...conversation }; }, "cc-preferences-form");
  });
  for (const id of ["cc-route-form", "cc-local-form", "cc-tools-form", "cc-native-form", "cc-gateway-form", "cc-preferences-form"]) {
    $("#" + id).addEventListener("input", () => { dirtyConfigurationForms.add(id); });
  }
  $("#cc-route-form").addEventListener("input", () => { routeFormDirty = true; $("#cc-route-dirty").textContent = "有未保存的修改"; });
  $("#cc-config-editor").addEventListener("input", () => { rawConfigDirty = true; $("#cc-config-dirty").textContent = "有未保存的修改"; });
  on("#cc-stop", () => stopConsole(true, true));
  $<HTMLFormElement>("#cc-api-key-form").addEventListener("submit", (event) => { event.preventDefault(); void changeApiKey(); });
  on("#cc-api-key-delete", () => changeApiKey(true));
  keyRoute.addEventListener("change", () => selectRoute(keyRoute.value));
  keyInput.addEventListener("input", updateControls);
  on("#cc-start", () => perform("正在读取钥匙串并启动 OpenCode…如出现 macOS 授权窗口，请完成授权。", async () => { await request("service.start"); await refreshStatus(); notice("服务启动请求已完成。"); }));
  on("#cc-service-stop", () => perform("正在停止服务…", async () => { await stopConsole(false); await request("service.stop"); serviceReady = false; updateControls(); notice("服务已停止。"); }));
  on("#cc-refresh-status", () => refreshStatus());
  on("#cc-reload", () => perform("正在重载配置…", async () => { if (!discardConfigurationDrafts()) return; await stopConsole(false); await request("service.reload"); resetConversationView(); await loadConfiguration(); await refreshFamily(); notice("配置已重载，点击“启动服务”继续。"); }));
  on("#cc-config-read", () => perform("正在读取配置…", async () => {
    if (!discardConfigurationDrafts()) return;
    await loadConfiguration(); notice("已读取 brain.json。");
  }));
  on("#cc-config-save", () => perform("正在保存配置…", async () => {
    if (dirtyConfigurationForms.size > 0 && !window.confirm("高级配置将替换各页表单尚未保存的设置，继续保存？")) return;
    const raw = $<HTMLTextAreaElement>("#cc-config-editor").value;
    if (raw.length > 200_000) throw new Error("配置内容过大。");
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("配置应为 JSON 对象。");
    await request("config.save", parsed); rawConfigDirty = false; dirtyConfigurationForms.clear(); $("#cc-config-dirty").textContent = "";
    await loadConfiguration(); notice("配置已保存。点击“重载配置”后生效，重载会结束当前设备互动。");
  }));
  on("#cc-models-status", () => perform("正在检查本机能力…", async () => {
    const result = object(await request("models.status")); latestModelHealth = result; renderCapabilities(container, result);
    $("#cc-models-json").textContent = pretty(result);
    notice(result.ready === true ? "已读取本地安装信息；环境就绪不代表推理效果已验证。" : errorText(result.error ?? "本地 Worker 尚未就绪。"), result.ready !== true);
    addActivity("已检查本地能力状态");
  }));
  on("#cc-models-unload", () => perform("正在释放本地模型…", async () => {
    await stopConsole(false); await request("models.unload");
    if (latestModelHealth) for (const model of Object.values(object(object(latestModelHealth.models).models))) object(model).loaded = false;
    renderCapabilities(container, latestModelHealth ?? undefined);
    $("#cc-models-json").textContent = "本地模型进程已退出；卡片保留上次检查的安装信息，下次明确请求时按需加载。";
    notice("已释放本地模型，安装状态仍为上次检查结果。"); addActivity("本地模型已释放");
  }));
  for (const tab of Array.from(container.querySelectorAll<HTMLButtonElement>(".cc-nav-item"))) tab.addEventListener("click", () => activatePanel(tab.dataset.panel ?? "talk"));
  $<HTMLFormElement>("#cc-chat-form").addEventListener("submit", (event) => { event.preventDefault(); void sendTurn(); });
  input.addEventListener("input", updateControls);
  input.addEventListener("keydown", (event) => { if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) { event.preventDefault(); void sendTurn(); } });
  profileSelect.addEventListener("change", () => { void changeProfile(profileSelect.value || null); });
  privateScope.addEventListener("change", () => { void (async () => {
    await stopConsole(false); resetConversationView();
    notice(privateScope.checked ? `已明确开启 ${profileName(selectedProfile)} 的个人私密范围。` : "已回到家庭共同对话。");
  })(); });
  on("#cc-new-conversation", async () => { await stopConsole(false); resetConversationView(); notice("已开始新对话。需要清理全部对话时，可前往“运行设置”。"); });
  on("#cc-forget-conversation", () => perform("正在忘记这段对话…", async () => {
    if (!window.confirm("忘记当前这段互动的原话及依赖记忆，并清理含有它的模型上下文？当天其他互动的原始记录会保留。")) { notice(""); return; }
    const id = conversationId; await stopConsole(false); await request("conversation.forget", { conversation_id: id }); resetConversationView(); notice("这段对话已忘记。");
  }));
  on("#cc-forget-all", () => perform("正在忘记所有对话…", async () => {
    if (!window.confirm("清理全部本地 conversation 及对应 OpenCode 会话？单独保存的记忆与档案会保留。")) { notice(""); return; }
    await stopConsole(false); await request("conversation.forget_all"); resetConversationView(); notice("所有对话已忘记。");
  }));
  on("#cc-record", () => toggleRecording("chat"));
  on("#cc-choose-wav", () => $<HTMLInputElement>("#cc-wav").click());
  on("#cc-draft-remove", () => { wavDraft = null; renderDraft(); input.focus(); });
  $<HTMLInputElement>("#cc-wav").addEventListener("change", () => {
    const picker = $<HTMLInputElement>("#cc-wav"); const file = picker.files?.[0]; picker.value = "";
    if (!file) return;
    const generation = ++mediaGeneration;
    void perform("正在读取语音文件…", async () => {
      const wav = await readWavFile(file);
      if (generation !== mediaGeneration) return;
      wavDraft = wav; renderDraft(); notice("语音文件已准备好，点击“发送”继续。");
    });
  });
  $<HTMLFormElement>("#cc-profile-form").addEventListener("submit", (event) => {
    event.preventDefault();
    void perform("正在创建家庭档案…", async () => {
      const name = $<HTMLInputElement>("#cc-profile-name").value.trim();
      await request("profile.create", { display_name: name, is_child: $<HTMLInputElement>("#cc-profile-child").checked });
      $<HTMLInputElement>("#cc-profile-name").value = ""; await refreshFamily(); notice("档案已创建。点击“选择此人”后用于本轮对话。");
    });
  });
  $<HTMLFormElement>("#cc-memory-form").addEventListener("submit", (event) => { event.preventDefault(); void perform("正在保存记忆…", saveMemory); });
  on("#cc-memory-reset", resetMemoryForm);
  $<HTMLSelectElement>("#cc-memory-scope").addEventListener("change", () => { memoryOwner.disabled = $<HTMLSelectElement>("#cc-memory-scope").value !== "profile"; });
  $<HTMLInputElement>("#cc-memory-filter").addEventListener("input", renderMemories);
  on("#cc-family-refresh", () => refreshFamily());
  identityKind.addEventListener("change", () => { clearSamples(); $<HTMLInputElement>("#cc-samples").accept = identityKind.value === "face" ? "image/jpeg,image/png" : ".wav,audio/wav"; updateControls(); });
  recognizeKind.addEventListener("change", () => { cancelCapture(); closeCamera(); $<HTMLInputElement>("#cc-recognize-input").accept = recognizeKind.value === "face" ? "image/jpeg,image/png" : ".wav,audio/wav"; updateControls(); });
  on("#cc-add-samples", () => { if (!selectedProfile) notice("请先选择要登记的家庭成员。", true); else $<HTMLInputElement>("#cc-samples").click(); });
  $<HTMLInputElement>("#cc-samples").addEventListener("change", () => {
    const picker = $<HTMLInputElement>("#cc-samples"); const files = Array.from(picker.files ?? []); picker.value = "";
    const kind = identityKind.value as IdentityKind; const generation = mediaGeneration;
    if (!files.length) return;
    void perform("正在读取本地登记样本…", async () => {
      if (!selectedProfile) throw new Error("请选择成员。");
      if (files.length + samples.length > maxSamples) throw new Error(`最多添加 ${maxSamples} 份样本。`);
      for (const file of files) {
        let base64: string;
        if (kind === "face") base64 = await readImageFile(file);
        else { const wav = await readWavFile(file); if (wav.seconds < 3) throw new Error("声纹样本至少需要 3 秒。"); base64 = wav.base64; }
        if (generation !== mediaGeneration) return;
        addSample({ base64, label: file.name });
      }
      notice("样本只保存在当前窗口，勾选同意并点击“登记样本”后才会提取特征。");
    });
  });
  on("#cc-clear-samples", clearSamples);
  on("#cc-sample-camera", () => openCamera("enroll"));
  on("#cc-sample-record", () => toggleRecording("enroll"));
  $<HTMLInputElement>("#cc-enroll-consent").addEventListener("change", updateControls);
  on("#cc-enroll", () => perform("正在本地提取并登记特征…", async () => {
    if (!selectedProfile || !$<HTMLInputElement>("#cc-enroll-consent").checked) throw new Error("请选择成员并明确同意本次登记。");
    const operation = ++identityOperation;
    const profileId = selectedProfile;
    const kind = identityKind.value;
    const result = await request("identity.enroll", { profile_id: selectedProfile, kind: identityKind.value,
      samples: samples.map((sample) => sample.base64), guardian_confirmed: true, replace_existing: $<HTMLInputElement>("#cc-enroll-replace").checked });
    if (operation !== identityOperation || disposed) return;
    if (selectedProfile === profileId && identityKind.value === kind) clearSamples();
    await refreshFamily(); notice(`${profileName(profileId)}的登记已保存，共 ${number(object(result).sample_count)} 份样本。识别仍只提供候选。`);
  }));
  on("#cc-recognize-file", () => $<HTMLInputElement>("#cc-recognize-input").click());
  $<HTMLInputElement>("#cc-recognize-input").addEventListener("change", () => {
    const picker = $<HTMLInputElement>("#cc-recognize-input"); const file = picker.files?.[0]; picker.value = "";
    const kind = recognizeKind.value as IdentityKind; const generation = mediaGeneration;
    if (!file) return;
    void perform("正在本地识别…", async () => {
      const media = kind === "face" ? await readImageFile(file) : (await readWavFile(file)).base64;
      if (generation !== mediaGeneration) return;
      await recognize(kind, media);
    });
  });
  on("#cc-recognize-camera", () => openCamera("recognize"));
  on("#cc-recognize-record", () => toggleRecording("recognize"));
  on("#cc-camera-close", closeCamera);
  cameraDialog.addEventListener("cancel", closeCamera);
  cameraDialog.addEventListener("close", closeCamera);
  on("#cc-camera-snapshot", async () => {
    try {
      const image = camera.snapshot(); const purpose = cameraPurpose; closeCamera();
      if (purpose === "enroll") addSample({ base64: image, label: `相机照片 · ${new Date().toLocaleTimeString()}` });
      else await perform("正在本地识别人脸…", () => recognize("face", image));
    } catch (error) { notice(errorText(error), true); }
  });
  on("#cc-credential-refresh", () => perform("正在读取设备凭据列表…", async () => { await refreshCredentials(); notice("列表已更新。"); }));
  on("#cc-credential-issue", () => perform("正在签发设备凭据…", async () => {
    const id = $<HTMLInputElement>("#cc-device-id").value.trim();
    if (!id) throw new Error("请填写设备 ID。");
    if (!window.confirm(`为设备 ${id} 签发新的 WSS 凭据？该设备原有凭据会立即失效。`)) { notice(""); return; }
    const generation = mediaGeneration;
    clearToken(); const result = object(await request("device.credentials.issue", { device_id: id }));
    if (generation !== mediaGeneration || disposed || !visible) {
      notice("凭据已签发，但本次显示已取消。需要查看时请重新轮换。"); return;
    }
    $("#cc-token-value").textContent = string(result.token); $("#cc-token-once").hidden = false;
    await refreshCredentials(); notice("新凭据仅在当前窗口显示一次，不写入浏览器存储。");
  }));
  on("#cc-token-dismiss", clearToken);

  function localCleanup(): void {
    keyInput.value = "";
    turnGeneration += 1; mediaGeneration += 1; recorder.cancel(); camera.close();
    stopPlayback(); householdController.stopMedia(); clearReplayClips(); clearToken(); samples = []; wavDraft = null;
  }
  const onPageHide = () => {
    const turn = activeTurn;
    activeTurn = null; turnBusy = false;
    localCleanup();
    if (turn) void request("assistant.cancel", { scope: "turn", turn_id: turn }).catch(() => {});
  };
  window.addEventListener("pagehide", onPageHide);
  // Status is metadata-only. Never poll model inference or microphone/camera.
  const statusTimer = window.setInterval(() => { if (visible && !turnBusy && !operationBusy) void refreshStatus(); }, 15_000);
  const permissionTimer = window.setInterval(() => { if (visible && (turnBusy || serviceReady)) { void refreshPermissions(); void refreshQuestions(); } }, 3_000);
    void listen<unknown>("brain:event", (event) => {
      const data = object(event.payload); const kind = string(data.kind);
      const labels: Record<string, string> = { "service.started": "对话服务已启动", "service.stopped": "对话服务已停止", "turn.started": "开始处理一次主动输入", "turn.completed": "本轮任务已完成", "turn.failed": "本轮任务未完成", "turn.cancelled": "本轮任务已取消", "harness_permission_required": "工具操作等待确认", "service.start_failed": "对话服务启动未完成" };
      if (labels[kind]) addActivity(labels[kind]);
      if (kind.includes("permission")) void refreshPermissions();
      if (kind.includes("question")) void refreshQuestions();
      if (["service.started", "service.stopped", "harness_started", "harness_stopped"].includes(kind)) { void refreshStatus(); void refreshPermissions(); }
      if (kind.startsWith("device.turn.") || kind.startsWith("device.presentation.")
        || kind.startsWith("device.playback.") || kind === "device.activity"
        || kind === "device.audio" || kind === "device.volume"
        || ["turn.started", "turn.completed", "turn.failed", "turn.cancelled"].includes(kind)) void refreshStatus();
      if (kind === "service.started") phase("idle", "服务已启动，可以开始对话。");
      if (kind === "service.stopped") {
        backendCancellable = false;
        cancelCapture(); closeCamera(); stopPlayback(); householdController.stopMedia(); phase("idle", "服务已停止。");
        activeDeviceTurn = null;
        resultDeviceTurn = null;
        $("#cc-device-turn").hidden = true;
        $("#cc-device-turn-result").replaceChildren();
        if (latestModelHealth) {
          for (const model of Object.values(object(object(latestModelHealth.models).models))) object(model).loaded = false;
          renderCapabilities(container, latestModelHealth);
        }
      }
      const eventTurn = string(data.turn_id);
      if (kind === "device.turn.started" && eventTurn) {
        backendCancellable = true; updateControls();
        const device = string(object(data.payload).device_id);
        activeDeviceTurn = { id: eventTurn, device, text: null, image: null };
        deviceToolEvents.clear();
        $("#cc-device-turn").hidden = false;
        $("#cc-device-turn-status").textContent = "正在处理设备上的请求";
        if (!device || resultDeviceTurn?.device !== device) {
          $("#cc-device-turn-result").replaceChildren();
          resultDeviceTurn = null;
        }
        resultDelivery.hidden = !resultDeviceTurn;
        $("#cc-device-tool-events").replaceChildren();
      } else if (kind === "device.display" && eventTurn) {
        const turn = activeDeviceTurn?.id === eventTurn ? activeDeviceTurn : resultDeviceTurn?.id === eventTurn ? resultDeviceTurn : null;
        const display = object(data.payload);
        if (turn && (display.status === "rendered" || display.status === "unavailable")
          && (display.part === "text" || display.part === "image")) {
          turn[display.part] = display.status;
          const label = deviceDeliveryText(turn.text, turn.image);
          if (turn === activeDeviceTurn) $("#cc-device-turn-status").textContent = label;
          if (turn === resultDeviceTurn) resultDelivery.textContent = label;
        }
      } else if (eventTurn && activeDeviceTurn?.id === eventTurn) {
        const status = $("#cc-device-turn-status");
        const routeMetric = harnessTimingMetric(data);
        if (routeMetric) $("#cc-device-tool-events").append(element("p", "cc-caption", routeMetric));
        if (kind === "turn.stage_completed") {
          const metric = turnStageMetric(data.payload);
          if (metric) $("#cc-device-tool-events").append(element("p", "cc-caption", metric));
        }
        if (kind === "harness_tool_activity") {
          const call = string(data.call_id), phase = string(data.phase), tool = string(data.tool);
          const labels: Record<string, string> = { "session.tool.called": "开始", "session.tool.success": "返回", "session.tool.failed": "失败" };
          const key = `${string(data.message_id)}:${call}:${phase}`;
          if (call && labels[phase] && !deviceToolEvents.has(key)) {
            deviceToolEvents.add(key);
            $("#cc-device-tool-events").append(element("p", "cc-caption", `${labels[phase]} · ${tool || "工具"} · ${call}`));
          }
        }
        if (kind === "turn.transcribing") status.textContent = "正在转写设备语音";
        else if (kind === "turn.thinking") status.textContent = "正在处理请求";
        else if (kind === "harness_permission_required") status.textContent = "等待在 Mac 上确认工具操作";
        else if (kind === "harness_question_required") status.textContent = "等待在 Mac 上回答问题";
        else if (kind === "harness_tool_activity" && data.phase === "session.tool.called") status.textContent = `正在使用工具 ${string(data.tool)}`;
        else if (kind === "harness_tool_activity" && data.phase === "session.tool.success") status.textContent = `工具已返回 ${string(data.tool)}`;
        else if (kind === "device.turn.failed" || kind === "turn.cancelled") status.textContent = kind === "turn.cancelled" ? "任务已停止" : errorText(object(data.payload).code);
        else if (kind === "device.turn.completed") {
          const payload = object(data.payload);
          const scene = object(payload.scene);
          const result = $("#cc-device-turn-result");
          resultDeviceTurn = activeDeviceTurn;
          const heading = payload.outcome === "needs_input" ? "最近一次回复 · 需要补充"
            : payload.outcome === "partial" ? "最近一次回复 · 部分完成" : "最近一次回复";
          result.replaceChildren(element("h3", "", heading), element("p", "cc-muted", string(payload.transcript)), element("p", "", string(scene.speech)));
          for (const value of list(scene.cards).slice(0, 4)) {
            const card = object(value);
            result.append(renderCard({ kind: string(card.kind), title: string(card.title),
              body: string(card.body), url: typeof card.url === "string" ? card.url : null }));
          }
          const imageData = string(payload.image_base64);
          if (imageData && imageData.length <= 90_000 && /^[A-Za-z0-9+/=]+$/.test(imageData)) {
            const image = element("img", "cc-card-image");
            image.alt = "设备收到的图片";
            image.src = `data:image/jpeg;base64,${imageData}`;
            result.append(image);
          }
          const metrics = deviceReplyMetrics(payload);
          if (metrics) result.append(element("p", "cc-caption", metrics));
          const timingDetails = renderTurnTimings(payload.timings);
          if (timingDetails) result.append(timingDetails);
          appendHouseholdLinks(result, payload);
          resultDelivery.textContent = deviceDeliveryText(activeDeviceTurn.text, activeDeviceTurn.image);
          resultDelivery.hidden = true;
          result.append(resultDelivery);
          status.textContent = resultDelivery.textContent;
        }
      }
      if (!eventTurn || eventTurn !== activeTurn) return;
      const routeMetric = harnessTimingMetric(data);
      if (routeMetric) addActivity(routeMetric);
      if (kind === "turn.transcribing") phase("thinking", "先在 Mac 上听懂这句话…");
      else if (kind === "turn.thinking") phase("thinking");
      else if (kind === "turn.synthesizing") phase("thinking", "正在用本地声音准备回复…");
      else if (kind === "harness_permission_required") phase("thinking", "有一项操作需要家长确认。");
      else if (kind === "harness_question_required") phase("thinking", "小呆在等你的回答。");
      else if (kind === "harness_tool_activity" && data.phase === "session.tool.called") {
        const tool = string(data.tool);
        if (tool) phase("thinking", `正在使用工具：${tool}`);
      }
      else if (kind === "turn.cancelled") { stopPlayback(); phase("idle", "这一次已经停止。"); }
    }).then((stop) => { if (disposed) stop(); else unsubscribe = stop; }).catch((error) => notice(errorText(error), true));
    void listen<unknown>("lan:event", (event) => {
      const kind = string(object(event.payload).kind);
      if (["bot_connected", "bot_disconnected", "server_started", "server_error"].includes(kind)) {
        void refreshStatus();
        if (kind === "bot_connected") addActivity("设备诊断连接已建立");
        if (kind === "bot_disconnected") addActivity("设备诊断连接已断开");
      }
    }).then((stop) => { if (disposed) stop(); else unsubscribeLan = stop; }).catch(() => {});
    void Promise.allSettled([refreshStatus(), loadConfiguration(), refreshFamily()]).then((results) => {
      if (disposed) return;
      for (const result of results) if (result.status === "rejected") { notice(errorText(result.reason), true); break; }
    });
  $("#cc-panel-family .cc-context-bar").append(button("合并重复档案", async () => {
    activatePanel("household");
    try { await householdController.showMerge(); } catch (error) { notice(errorText(error), true); }
  }));
  renderWelcome(); renderSamples(); resetMemoryForm(); renderCapabilities(container); addActivity("控制台已打开"); updateControls();
  let initialPanel = "talk";
  try { initialPanel = localStorage.getItem("espbot.console.panel") ?? "talk"; } catch { /* Use the daily conversation page. */ }
  activatePanel(CONSOLE_PANELS.some((page) => page.id === initialPanel) ? initialPanel : "talk");

  return {
    setVisible(next: boolean): void {
      visible = next;
      if (!visible) { keyInput.value = ""; cancelCapture(); closeCamera(); stopPlayback(); clearToken(); }
      else { void refreshStatus(); void refreshPermissions(); }
      householdController.setPanel(panel, visible);
    },
    dispose(): void {
      if (disposed) return;
      disposed = true; localCleanup(); householdController.dispose(); unsubscribe?.(); unsubscribeLan?.();
      window.clearInterval(statusTimer); window.clearInterval(permissionTimer);
      window.removeEventListener("pagehide", onPageHide);
    },
  };
}
