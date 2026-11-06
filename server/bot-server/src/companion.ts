import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { LocalCamera, PcmRecorder, base64ToBuffer, readImageFile, readWavFile, type CapturedWav } from "./capture";
import "./companion.css";

type JsonObject = Record<string, unknown>;
type Profile = { id: string; display_name: string; is_child: boolean };
type MemoryRecord = {
  id: string; scope: "household" | "profile"; profile_id: string | null; content: string;
  category: string; source: string; source_ref: string | null; confirmed: boolean;
  allow_cloud: boolean; expires_at_ms: number | null;
};
type Enrollment = { profile_id: string; kind: string; model_id: string; sample_count: number };
type Card = { kind: string; title: string; body: string; url: string | null };
type Scene = { speech: string; expression: string; cards: Card[] };
type TurnResult = {
  turn_id: string; conversation_id: string; transcript: string; scene: Scene;
  provider_id: string; model_id: string; audio: unknown; identity_candidate: unknown; warnings: string[];
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
  if (text.includes("NotAllowedError") || text.includes("Permission denied")) return "没有获得麦克风或摄像头权限。可在 macOS 设置里开启，或选择本地文件。";
  if (text.includes("NotFoundError")) return "没有找到可用的麦克风或摄像头。可选择本地文件继续。";
  if (text.includes("capture_cancelled")) return "已取消采集。";
  return text.slice(0, 2000);
}

export function mountCompanion(container: HTMLElement, options: { mock?: boolean } = {}): CompanionController {
  // Static application template only. Every model, file, profile and tool value
  // below is assigned with textContent / DOM properties, never interpolated HTML.
  container.innerHTML = `
    <div class="cc-shell">
      <header class="cc-head">
        <div class="cc-pet" id="cc-pet" data-expression="idle" aria-hidden="true"><i></i><i></i><b></b></div>
        <div class="cc-heading"><span class="cc-kicker">MAC 交互台</span><h1>小呆 <span>在这里</span></h1><p id="cc-phase" role="status">聊一聊，或者一起发现一个小原理。</p></div>
        <div class="cc-head-actions"><span class="cc-pill" id="cc-service-pill">正在读取状态</span><button type="button" class="cc-button primary" id="cc-start">启动服务</button><button type="button" class="cc-button stop" id="cc-stop">■ 停止</button></div>
      </header>
      <nav class="cc-tabs" aria-label="小呆功能"><button type="button" class="selected" data-panel="talk" aria-selected="true">对话</button><button type="button" data-panel="family" aria-selected="false">家庭与记忆</button><button type="button" data-panel="settings" aria-selected="false">服务与设置</button></nav>
      <p id="cc-notice" class="cc-notice" role="status" aria-live="polite" hidden></p>

      <div class="cc-conversation-bar"><label for="cc-profile">本轮成员</label><select id="cc-profile"><option value="">未指定 · 只用共享记忆</option></select><button type="button" class="cc-button quiet" id="cc-new-conversation">新对话</button><button type="button" class="cc-button quiet" id="cc-forget-conversation">忘记这段</button></div>
      <section class="cc-panel cc-talk" id="cc-panel-talk">
        <div class="cc-messages" id="cc-messages" role="log" aria-live="polite" aria-relevant="additions text"></div>
        <div id="cc-permissions" class="cc-permissions" aria-label="待家长确认的操作" hidden></div>
        <div class="cc-draft" id="cc-draft" hidden><span id="cc-draft-label"></span><button type="button" class="cc-button quiet" id="cc-draft-remove">移除</button></div>
        <form id="cc-chat-form" class="cc-composer"><textarea id="cc-input" maxlength="4000" rows="2" autocomplete="off" placeholder="给小呆一个问题，或者一起想个有趣的玩法…" aria-label="发给小呆的话"></textarea><div class="cc-composer-bottom"><div class="cc-input-actions"><button type="button" class="cc-button" id="cc-record">● 录一段</button><button type="button" class="cc-button quiet" id="cc-choose-wav">选择 WAV</button><input type="file" id="cc-wav" accept=".wav,audio/wav,audio/x-wav" hidden><label class="cc-check"><input type="checkbox" id="cc-speak" checked>朗读回复</label></div><button type="submit" class="cc-button primary" id="cc-send">发送 ↑</button></div><p class="cc-caption" id="cc-capture-hint">按下才录音，最长 30 秒；结束后点“发送”。不用连接机器人。</p></form>
      </section>

      <section class="cc-panel cc-scroll" id="cc-panel-family" hidden>
        <div class="cc-family-grid">
          <section class="cc-box"><h2>家庭成员</h2><p class="cc-muted">只有明确选择的成员，才会使用对应的个人记忆。识别结果不会自动切换身份。</p><form id="cc-profile-form" class="cc-form"><label>新成员昵称<input id="cc-profile-name" maxlength="80" required placeholder="例如：小米"></label><label class="cc-check"><input type="checkbox" id="cc-profile-child" checked>儿童档案（手动指定）</label><button class="cc-button" type="submit">创建档案</button></form><div id="cc-profile-list" class="cc-row-list"></div></section>
          <section class="cc-box"><h2>登记人脸 / 声纹</h2><p class="cc-muted" id="cc-enrollment-owner">请先在“本轮成员”中选择一个档案。</p><div class="cc-inline"><select id="cc-identity-kind" aria-label="登记类型"><option value="face">人脸</option><option value="speaker">声纹</option></select><button type="button" class="cc-button" id="cc-add-samples">选择样本</button><button type="button" class="cc-button quiet" id="cc-sample-camera">拍一张</button><button type="button" class="cc-button quiet" id="cc-sample-record">录一段样本</button><input type="file" id="cc-samples" accept="image/jpeg,image/png" multiple hidden></div><p class="cc-caption" id="cc-sample-guidance">至少 3 张不同照片，每张只有本人。样本留在本机。</p><div id="cc-sample-list" class="cc-sample-list"></div><label class="cc-check"><input type="checkbox" id="cc-enroll-consent">我是家长 / 本人，明确同意本次登记</label><label class="cc-check"><input type="checkbox" id="cc-enroll-replace">替换该成员原来的同类登记</label><div class="cc-inline"><button type="button" class="cc-button primary" id="cc-enroll">登记样本</button><button type="button" class="cc-button quiet" id="cc-clear-samples">清除待登记样本</button></div><div id="cc-enrollment-list" class="cc-row-list"></div></section>
          <section class="cc-box"><h2>识别一次</h2><p class="cc-muted">本地提取特征并寻找已登记候选。相似度不是正确率；未知或多人照片会明确显示。</p><div class="cc-inline"><select id="cc-recognize-kind" aria-label="识别类型"><option value="face">人脸</option><option value="speaker">声纹</option></select><button type="button" class="cc-button" id="cc-recognize-file">选择文件并识别</button><button type="button" class="cc-button quiet" id="cc-recognize-camera">拍照识别</button><button type="button" class="cc-button quiet" id="cc-recognize-record">录音识别</button><input type="file" id="cc-recognize-input" accept="image/jpeg,image/png" hidden></div><div id="cc-identity-result" class="cc-identity-result">尚未请求识别。</div></section>
          <section class="cc-box"><h2 id="cc-memory-form-title">记住一件小事</h2><p class="cc-muted">当前对话使用云端模型。只有“已确认”并“允许提供给云端”的记忆才参与回答。</p><form id="cc-memory-form" class="cc-form"><div class="cc-two-fields"><label>归属<select id="cc-memory-scope"><option value="household">家庭共享</option><option value="profile">某个成员</option></select></label><label>成员<select id="cc-memory-owner"><option value="">请选择成员</option></select></label></div><label>内容<textarea id="cc-memory-content" rows="3" maxlength="2000" required placeholder="例如：喜欢用恐龙做数学题的例子。"></textarea></label><div class="cc-two-fields"><label>分类<input id="cc-memory-category" value="preference" maxlength="64" required></label><label>来源<select id="cc-memory-source"><option value="guardian_note">家长填写</option><option value="user_statement">本人表达</option><option value="assistant_suggestion">助手建议待检查</option></select></label></div><label>来源备注（可选）<input id="cc-memory-ref" maxlength="256" placeholder="例如：今天一起聊天时确认"></label><label>到期时间（留空则长期）<input id="cc-memory-expiry" type="datetime-local"></label><label class="cc-check"><input type="checkbox" id="cc-memory-confirmed">我已确认内容</label><label class="cc-check"><input type="checkbox" id="cc-memory-cloud">允许将这一条提供给云端模型</label><div class="cc-inline"><button class="cc-button primary" type="submit" id="cc-memory-save">保存记忆</button><button type="button" class="cc-button quiet" id="cc-memory-reset">清空表单</button></div></form></section>
        </div>
        <section class="cc-box cc-memory-box"><div class="cc-section-title"><h2>记忆清单 <span id="cc-memory-count"></span></h2><input id="cc-memory-filter" type="search" placeholder="搜索内容 / 成员" aria-label="筛选记忆"><button type="button" class="cc-button quiet" id="cc-family-refresh">刷新</button></div><div id="cc-memory-list" class="cc-memory-list"></div></section>
      </section>

      <section class="cc-panel cc-scroll" id="cc-panel-settings" hidden>
        <section class="cc-box"><div class="cc-section-title"><h2>服务</h2><button type="button" class="cc-button quiet" id="cc-refresh-status">刷新状态</button><button type="button" class="cc-button quiet" id="cc-service-stop">停止服务</button><button type="button" class="cc-button quiet" id="cc-reload">重载配置</button></div><p class="cc-muted" id="cc-status-summary">正在读取本地服务状态。</p><div class="cc-routes" id="cc-routes"></div><p class="cc-caption">优先级按配置顺序使用。没有服务商返回的剩余额度时，不显示估算余额。</p><h3>继承的工具</h3><div id="cc-mcp" class="cc-row-list"></div><details><summary>完整状态（不含密钥）</summary><pre id="cc-status-json" class="cc-json"></pre></details></section>
        <section class="cc-box"><div class="cc-section-title"><h2>本地语音与视觉</h2><button type="button" class="cc-button" id="cc-models-status">查看模型状态</button><button type="button" class="cc-button quiet" id="cc-models-unload">卸载已加载模型</button></div><p class="cc-muted">ASR、人脸和声纹在 Mac 本地运行；第一次使用前，需要按文档准备依赖与模型。这里不会自动下载。</p><pre id="cc-models-json" class="cc-json">尚未请求模型状态。</pre></section>
        <section class="cc-box"><div class="cc-section-title"><h2>配置 brain.json</h2><button type="button" class="cc-button quiet" id="cc-config-read">重新读取</button><button type="button" class="cc-button primary" id="cc-config-save">保存并重载</button></div><p class="cc-muted">配置模型顺序、环境变量名称、本地模型路径和允许展示的媒体域名。修改后服务会停止，点击“启动服务”继续。</p><p class="cc-caption" id="cc-config-path"></p><textarea id="cc-config-editor" class="cc-config-editor" rows="20" spellcheck="false" aria-label="brain.json 配置"></textarea></section>
        <details class="cc-box"><summary>设备安全连接凭据</summary><p class="cc-muted">供支持 v3 的设备使用，需要 WSS 证书信任。旧版 LAN 诊断配对不等于安全音频连接。</p><div class="cc-inline"><input id="cc-device-id" maxlength="64" placeholder="设备 ID" aria-label="设备 ID"><button type="button" class="cc-button" id="cc-credential-issue">签发 / 轮换凭据</button><button type="button" class="cc-button quiet" id="cc-credential-refresh">刷新列表</button></div><div id="cc-token-once" class="cc-token" hidden><strong>仅本次显示，请通过受信任的方式写入设备。</strong><code id="cc-token-value"></code><button type="button" class="cc-button quiet" id="cc-token-dismiss">隐藏并清除</button></div><div id="cc-credential-list" class="cc-row-list"></div></details>
        <section class="cc-box"><h2>忘记对话</h2><p class="cc-muted">清除本地 conversation 记录和对应的 OpenCode 会话，保留单独保存的记忆与档案。</p><button type="button" class="cc-button danger" id="cc-forget-all">忘记所有对话</button></section>
      </section>
      <dialog id="cc-camera-dialog" class="cc-camera-dialog"><h2 id="cc-camera-title">本地相机预览</h2><p class="cc-muted">画面只留在本机。拍摄后会关闭摄像头。</p><video id="cc-camera-video" autoplay muted playsinline></video><div class="cc-inline"><button type="button" class="cc-button primary" id="cc-camera-snapshot">拍摄</button><button type="button" class="cc-button quiet" id="cc-camera-close">关闭相机</button></div></dialog>
    </div>`;

  const $ = <T extends HTMLElement>(selector: string): T => {
    const node = container.querySelector<T>(selector);
    if (!node) throw new Error(`Missing companion element ${selector}`);
    return node;
  };
  const input = $<HTMLTextAreaElement>("#cc-input");
  const profileSelect = $<HTMLSelectElement>("#cc-profile");
  const memoryOwner = $<HTMLSelectElement>("#cc-memory-owner");
  const identityKind = $<HTMLSelectElement>("#cc-identity-kind");
  const recognizeKind = $<HTMLSelectElement>("#cc-recognize-kind");
  const messages = $("#cc-messages");
  const recorder = new PcmRecorder();
  const camera = new LocalCamera();
  const cameraDialog = $<HTMLDialogElement>("#cc-camera-dialog");
  let visible = true;
  let disposed = false;
  let panel = "talk";
  let profiles: Profile[] = [];
  let memories: MemoryRecord[] = [];
  let enrollments: Enrollment[] = [];
  let selectedProfile: string | null = null;
  let conversationId = newId("console");
  let activeTurn: string | null = null;
  let turnGeneration = 0;
  let mediaGeneration = 0;
  let turnBusy = false;
  let operationBusy = false;
  let statusBusy = false;
  let permissionBusy = false;
  let serviceReady = false;
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
  const replayClips = new Map<string, { base64: string; control: HTMLButtonElement }>();
  let unsubscribe: UnlistenFn | null = null;

  const request = async <T = unknown>(method: string, payload: unknown = {}): Promise<T> => {
    if (options.mock) throw new Error("当前是旧版界面预览模式；小呆服务没有被调用。请在 Tauri 应用中使用。");
    return invoke<T>("companion_request", { method, payload });
  };

  function notice(text: string, error = false): void {
    if (disposed) return;
    const node = $("#cc-notice");
    node.hidden = !text;
    node.classList.toggle("error", error);
    node.textContent = text;
  }
  function phase(value: Phase, description?: string, expression?: string): void {
    const defaults: Record<Phase, string> = {
      idle: "聊一聊，或者一起发现一个小原理。", listening: "正在录音，只听你这一次主动说的话。",
      thinking: "我想一想，也查一查需要的资料…", speaking: "小呆正在说话。", error: "这一步没完成，可以看看提示。",
    };
    $("#cc-phase").textContent = description ?? defaults[value];
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
    if (!["talk", "family", "settings"].includes(next)) return;
    if (panel !== next) { cancelCapture(); closeCamera(); }
    panel = next;
    for (const tab of Array.from(container.querySelectorAll<HTMLButtonElement>(".cc-tabs button"))) {
      const active = tab.dataset.panel === next;
      tab.classList.toggle("selected", active); tab.setAttribute("aria-selected", String(active));
    }
    for (const candidate of ["talk", "family", "settings"]) $("#cc-panel-" + candidate).hidden = candidate !== next;
    if (next === "settings") void refreshStatus();
  }
  function updateControls(): void {
    const blocked = turnBusy || operationBusy || captureConverting || options.mock === true;
    $<HTMLButtonElement>("#cc-send").disabled = blocked || !serviceReady || recordPurpose !== null || (!wavDraft && !input.value.trim());
    input.disabled = turnBusy || wavDraft !== null;
    profileSelect.disabled = blocked || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-start").disabled = blocked || serviceReady;
    $<HTMLButtonElement>("#cc-record").disabled = blocked || (recordPurpose !== null && recordPurpose !== "chat");
    $<HTMLButtonElement>("#cc-choose-wav").disabled = blocked || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-record").textContent = recordPurpose === "chat" ? "■ 结束录音" : "● 录一段";
    $<HTMLButtonElement>("#cc-enroll").disabled = blocked || recordPurpose !== null || !selectedProfile
      || samples.length < minSamples || !$<HTMLInputElement>("#cc-enroll-consent").checked;
    $<HTMLButtonElement>("#cc-sample-camera").disabled = blocked || identityKind.value !== "face" || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-sample-record").disabled = blocked || identityKind.value !== "speaker"
      || (recordPurpose !== null && recordPurpose !== "enroll");
    $<HTMLButtonElement>("#cc-sample-record").textContent = recordPurpose === "enroll" ? "■ 结束样本录音" : "录一段样本";
    $<HTMLButtonElement>("#cc-add-samples").disabled = blocked || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-clear-samples").disabled = blocked || recordPurpose !== null;
    for (const control of Array.from(container.querySelectorAll<HTMLButtonElement>("#cc-sample-list button"))) control.disabled = blocked || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-recognize-file").disabled = blocked || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-recognize-camera").disabled = blocked || recognizeKind.value !== "face" || recordPurpose !== null;
    $<HTMLButtonElement>("#cc-recognize-record").disabled = blocked || recognizeKind.value !== "speaker"
      || (recordPurpose !== null && recordPurpose !== "recognize");
    $<HTMLButtonElement>("#cc-recognize-record").textContent = recordPurpose === "recognize" ? "■ 结束并识别" : "录音识别";
    identityKind.disabled = blocked || recordPurpose !== null;
    recognizeKind.disabled = blocked || recordPurpose !== null;
    $("#cc-enrollment-owner").textContent = selectedProfile ? `登记给：${profileName(selectedProfile)}。请确认样本都是本人。` : "请先在“本轮成员”中选择一个档案。";
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
  }
  function clearReplayClips(): void {
    for (const clip of replayClips.values()) { clip.control.disabled = true; clip.control.textContent = "音频已释放"; }
    replayClips.clear();
  }
  function closeCamera(): void { cameraRequestGeneration += 1; camera.close(); if (cameraDialog.open) cameraDialog.close(); }
  function cancelCapture(): void {
    const hadCapture = recordPurpose !== null || captureConverting;
    mediaGeneration += 1;
    recorder.cancel(); recordPurpose = null; captureConverting = false;
    $("#cc-capture-hint").textContent = "按下才录音，最长 30 秒；结束后点“发送”。不用连接机器人。";
    if (hadCapture && !turnBusy) phase("idle", "本地采集已停止。");
    updateControls();
  }
  async function stopEverything(show = true): Promise<void> {
    turnGeneration += 1;
    const turn = activeTurn;
    activeTurn = null; turnBusy = false;
    cancelCapture(); closeCamera(); stopPlayback(); clearToken();
    wavDraft = null; renderDraft(); phase("idle", "已停止本地采集和播放。");
    updateControls();
    try { await request("assistant.cancel", turn ? { turn_id: turn } : {}); if (show) notice("已停止本轮互动。"); }
    catch (error) { if (show) notice(`本地采集和播放已停止；后端取消结果：${errorText(error)}`, true); }
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
      element("p", "", "文字、声音和屏幕卡片一起用。机器人还没到，也可以先在 Mac 上聊。"));
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
  async function playReply(base64: string, host: HTMLElement, generation: number): Promise<void> {
    if (disposed || !visible || generation !== turnGeneration) return;
    stopPlayback();
    try {
      const url = URL.createObjectURL(new Blob([base64ToBuffer(base64, 24 * 1024 * 1024)], { type: "audio/wav" }));
      const player = element("audio", "cc-reply-audio"); player.controls = true; player.preload = "auto"; player.src = url;
      audio = player; audioUrl = url; host.append(player);
      player.addEventListener("playing", () => { if (audio === player) phase("speaking"); });
      player.addEventListener("ended", () => { if (audio === player) { stopPlayback(); if (!turnBusy) phase("idle", "还想接着聊哪一点？", "happy"); } });
      await player.play();
    } catch { notice("回复已经显示。浏览器没有自动播放声音时，可点击“播放回复”。"); }
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
        turn_id: turnId, conversation_id: currentConversation, profile_id: selectedProfile,
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
      const wav = string(object(result.audio).wav_base64);
      if (wav) {
        const audioHost = element("div", "cc-audio-host");
        const play = button("播放回复", async () => {
          const clip = replayClips.get(turnId);
          if (clip) await playReply(clip.base64, audioHost, turnGeneration);
        });
        replayClips.set(turnId, { base64: wav, control: play });
        while (replayClips.size > 3) {
          const oldest = replayClips.keys().next().value as string | undefined;
          if (!oldest) break;
          const old = replayClips.get(oldest);
          if (old) { old.control.disabled = true; old.control.textContent = "音频已释放"; }
          replayClips.delete(oldest);
        }
        reply.row.append(play, audioHost);
        if ($<HTMLInputElement>("#cc-speak").checked && visible) void playReply(wav, audioHost, generation);
      }
      if (result.identity_candidate) renderIdentity(result.identity_candidate, true);
      if (result.warnings?.length) notice(result.warnings.map(errorText).join("\n"));
      phase("idle", "还想接着聊哪一点？", result.scene.expression);
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
    if (turnBusy || operationBusy || captureConverting || recordPurpose || options.mock) return;
    if (purpose === "enroll" && !selectedProfile) { notice("请先选择要登记的家庭成员。", true); return; }
    stopPlayback(); closeCamera();
    const generation = ++mediaGeneration;
    recordPurpose = purpose; phase("listening"); updateControls();
    try {
      await recorder.start({
        progress: (seconds) => {
          if (generation !== mediaGeneration) return;
          const text = `正在录音 ${seconds.toFixed(1)} / 30 秒 · 再按一次结束`;
          $("#cc-capture-hint").textContent = text; $("#cc-phase").textContent = text;
        },
        limit: () => { if (generation === mediaGeneration) void finishRecording(); },
      });
    } catch (error) {
      if (generation === mediaGeneration) { cancelCapture(); notice(errorText(error), true); phase("idle"); }
    }
  }
  async function finishRecording(): Promise<void> {
    const purpose = recordPurpose;
    if (!purpose || !recorder.recording) return;
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
    } catch (error) { if (generation === mediaGeneration) notice(errorText(error), true); }
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
    try {
      await camera.open($<HTMLVideoElement>("#cc-camera-video"));
      if (generation === cameraRequestGeneration && cameraDialog.open) $<HTMLButtonElement>("#cc-camera-snapshot").disabled = false;
    } catch (error) {
      if (generation === cameraRequestGeneration) { closeCamera(); notice(errorText(error), true); }
    }
  }
  async function recognize(kind: IdentityKind, media: string): Promise<void> {
    const generation = mediaGeneration;
    const result = await request("identity.recognize", { kind, ...(kind === "face" ? { image_base64: media } : { wav_base64: media }) });
    if (generation !== mediaGeneration || disposed) return;
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
    profileSelect.replaceChildren(new Option("未指定 · 只用共享记忆", ""));
    memoryOwner.replaceChildren(new Option("请选择成员", ""));
    const host = $("#cc-profile-list"); host.replaceChildren();
    for (const profile of profiles) {
      profileSelect.add(new Option(profile.display_name, profile.id));
      memoryOwner.add(new Option(profile.display_name, profile.id));
      const row = element("div", "cc-manage-row");
      const meta = element("div", "cc-manage-meta");
      meta.append(element("strong", "", profile.display_name), element("span", "cc-caption", profile.is_child ? "儿童档案 · 手动指定" : "家庭成员"));
      const actions = element("div", "cc-inline");
      actions.append(button(profile.id === selectedProfile ? "本轮已选择" : "选择此人", () => changeProfile(profile.id)),
        button("删除", () => perform("正在删除档案与关联数据…", async () => {
          if (!window.confirm(`删除“${profile.display_name}”的档案、关联记忆、声纹和人脸登记？对应对话也会清理。`)) { notice(""); return; }
          await stopEverything(false); await request("profile.delete", { profile_id: profile.id });
          if (selectedProfile === profile.id) selectedProfile = null;
          clearSamples(); resetMemoryForm(); resetConversationView(); await refreshFamily();
          notice("档案与关联数据已删除。");
        }), "cc-button danger"));
      row.append(meta, actions); host.append(row);
    }
    profileSelect.value = selectedProfile ?? "";
    memoryOwner.value = editingMemory?.profile_id ?? (previousOwner || selectedProfile || "");
    if (!profiles.length) host.append(element("p", "cc-muted", "还没有家庭成员档案。可以先匿名聊天。"));
    updateControls();
  }
  async function refreshFamily(): Promise<void> {
    const results = await Promise.allSettled([
      request<Profile[]>("profile.list"), request<MemoryRecord[]>("memory.list"), request<Enrollment[]>("identity.list"),
    ]);
    if (disposed) return;
    const [people, notes, identities] = results;
    if (people.status === "fulfilled") {
      profiles = people.value;
      if (selectedProfile && !profiles.some((profile) => profile.id === selectedProfile)) {
        await stopEverything(false); selectedProfile = null; clearSamples(); resetMemoryForm(); resetConversationView();
      }
      renderProfiles();
    }
    if (notes.status === "fulfilled") { memories = notes.value; renderMemories(); }
    if (identities.status === "fulfilled") { enrollments = identities.value; renderEnrollments(); }
    for (const result of results) if (result.status === "rejected") { notice(errorText(result.reason), true); break; }
  }
  async function changeProfile(id: string | null): Promise<void> {
    if (id === selectedProfile) return;
    if (id && !profiles.some((profile) => profile.id === id)) return;
    await stopEverything(false);
    selectedProfile = id; clearSamples(); resetMemoryForm(); resetConversationView(); renderProfiles(); renderEnrollments();
    $("#cc-identity-result").textContent = "本轮成员已改变，尚未重新请求识别。";
    notice(id ? `已明确选择 ${profileName(id)}，开始一段新对话。` : "已切换为未指定成员，开始一段新对话。");
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
    $("#cc-memory-count").textContent = `(${shown.length})`;
    const host = $("#cc-memory-list"); host.replaceChildren();
    for (const memory of shown.slice(0, 100)) {
      const card = element("article", "cc-memory-item");
      const tags = element("div", "cc-memory-tags");
      tags.append(element("span", "cc-tag", memory.scope === "household" ? "家庭共享" : profileName(memory.profile_id)),
        element("span", `cc-tag ${memory.confirmed ? "good" : "pending"}`, memory.confirmed ? "已确认" : "待确认"),
        element("span", "cc-tag", memory.allow_cloud ? "允许云端" : "仅本地"));
      card.append(tags, element("p", "cc-memory-text", memory.content));
      const meta = element("div", "cc-memory-footer");
      meta.append(element("span", "cc-caption", `${memory.category}${memory.expires_at_ms ? ` · 到期 ${new Date(memory.expires_at_ms).toLocaleString()}` : ""}`));
      const actions = element("div", "cc-inline");
      actions.append(button("编辑", () => editMemory(memory)), button("删除", () => perform("正在删除记忆…", async () => {
        if (!window.confirm(`删除这条记忆，并清理对话中的旧副本？\n\n${memory.content.slice(0, 200)}`)) { notice(""); return; }
        await stopEverything(false); await request("memory.delete", { id: memory.id });
        resetConversationView(); if (editingMemory?.id === memory.id) resetMemoryForm(); await refreshFamily(); notice("记忆已删除，对话已重新开始。");
      }), "cc-button danger"));
      meta.append(actions); card.append(meta); host.append(card);
    }
    if (!shown.length) host.append(element("p", "cc-muted", query ? "没有匹配的记忆。" : "还没有保存的记忆。记录一些有帮助的小偏好就够了。"));
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
    $("#cc-memory-form").scrollIntoView({ behavior: "smooth", block: "center" });
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
    await stopEverything(false); await request("memory.save", payload);
    resetMemoryForm(); resetConversationView(); await refreshFamily(); notice("记忆已保存，对话已重新开始以应用新内容。");
  }
  async function refreshStatus(): Promise<void> {
    if (statusBusy || disposed || options.mock) return;
    statusBusy = true;
    try {
      const value = object(await request("service.status"));
      if (disposed) return;
      const harness = object(value.harness);
      serviceReady = value.ready === true && value.accepting_turns === true && harness.healthy === true;
      const pill = $("#cc-service-pill");
      pill.textContent = serviceReady ? "服务已启动" : value.ready === false ? "配置待处理" : "服务未启动";
      pill.classList.toggle("ready", serviceReady);
      $("#cc-status-summary").textContent = value.ready === false ? string(value.error, "配置未完成")
        : `${serviceReady ? "可以开始对话" : "先完成配置并启动服务"} · OpenCode ${string(harness.version, string(harness.source_version, "版本待读取"))} · ${string(harness.mode, "managed")}`;
      $("#cc-status-json").textContent = pretty(value);
      $("#cc-config-path").textContent = string(value.config_path);
      renderRoutes(list(harness.routes));
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
      if (!disposed) { serviceReady = false; $("#cc-service-pill").textContent = "状态不可用"; $("#cc-status-summary").textContent = errorText(error); updateControls(); }
    } finally { statusBusy = false; }
  }
  function renderRoutes(routes: unknown[]): void {
    const host = $("#cc-routes"); host.replaceChildren();
    routes.forEach((value, index) => {
      const route = object(value);
      const card = element("article", "cc-route");
      card.append(element("span", "cc-route-order", String(index + 1)), element("strong", "", string(route.label, string(route.id))), element("code", "", string(route.model_id)));
      let state = route.enabled === false ? "已停用" : route.credential_env_present === false ? "等待配置密钥" : "按顺序可用";
      if (route.cooling_down === true) state = `暂时跳过 · ${new Date(number(route.retry_at_ms)).toLocaleTimeString()} 后再尝试`;
      card.append(element("span", "cc-caption", state));
      if (route.last_failure) card.append(element("span", "cc-caption", string(route.last_failure)));
      if (route.cooling_down === true) card.append(button("重新尝试此路线", () => perform("正在清除该路线的冷却状态…", async () => {
        await request("harness.route.reset", { route_id: string(route.id) }); await refreshStatus(); notice("冷却状态已清除，下次对话按模型顺序重试。");
      })));
      host.append(card);
    });
  }
  async function loadConfiguration(): Promise<void> {
    const config = object(await request("config.read"));
    if (disposed) return;
    $<HTMLTextAreaElement>("#cc-config-editor").value = pretty(config, 200_000);
    approvedMediaHosts = new Set(list(object(config.conversation).media_hosts).map((value) => string(value).toLowerCase()).filter(Boolean));
    $<HTMLInputElement>("#cc-speak").checked = object(config.conversation).default_speak !== false;
    minSamples = Math.min(20, Math.max(3, number(object(config.memory).min_enrollment_samples, 3)));
    maxSamples = Math.min(32, Math.max(minSamples, number(object(config.memory).max_enrollment_samples, 12)));
    updateControls();
  }
  async function refreshPermissions(): Promise<void> {
    if (permissionBusy || disposed || options.mock) return;
    permissionBusy = true;
    try {
      const permissions = list(await request("harness.permissions"));
      if (disposed) return;
      const host = $("#cc-permissions"); host.replaceChildren(); host.hidden = permissions.length === 0;
      for (const value of permissions.slice(0, 16)) {
        const permission = object(value);
        const name = string(permission.action, string(permission.permission, "工具操作"));
        const details = { action: name, description: permission.description, patterns: permission.patterns, input: permission.input };
        const encoded = JSON.stringify(details, null, 2) ?? "{}";
        const card = element("section", "cc-permission");
        card.append(element("strong", "", `请家长确认：${name}`), element("pre", "cc-json", encoded.length <= 24_000 ? encoded : `${encoded.slice(0, 24_000)}\n参数过长，不能在此批准。`));
        const actions = element("div", "cc-inline");
        const reply = async (allow: boolean) => {
          if (allow && !window.confirm(`仅允许这一次具体操作？\n\n${name}\n请确认已读完卡片中的目标与参数。`)) return;
          for (const control of Array.from(actions.querySelectorAll<HTMLButtonElement>("button"))) control.disabled = true;
          try {
            await request("harness.permission.reply", { session_id: string(permission.session_id), request_id: string(permission.request_id), allow });
            notice(allow ? "已仅允许这次操作。" : "已拒绝这次操作。");
          } catch (error) { notice(errorText(error), true); }
          finally { void refreshPermissions(); }
        };
        const allow = button("仅允许这一次", () => reply(true), "cc-button primary");
        allow.disabled = encoded.length > 24_000;
        actions.append(allow, button("拒绝", () => reply(false), "cc-button danger")); card.append(actions); host.append(card);
      }
      if (permissions.length && panel !== "talk") notice("有一项工具操作等待家长确认，请切换到“对话”查看具体内容。");
    } catch (error) { if (turnBusy) notice(errorText(error), true); }
    finally { permissionBusy = false; }
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
  on("#cc-stop", () => stopEverything());
  on("#cc-start", () => perform("正在启动 OpenCode…", async () => { await request("service.start"); await refreshStatus(); notice("服务启动请求已完成。"); }));
  on("#cc-service-stop", () => perform("正在停止服务…", async () => { await stopEverything(false); await request("service.stop"); serviceReady = false; updateControls(); notice("服务已停止。"); }));
  on("#cc-refresh-status", () => refreshStatus());
  on("#cc-reload", () => perform("正在重载配置…", async () => { await stopEverything(false); await request("service.reload"); resetConversationView(); await loadConfiguration(); await refreshFamily(); notice("配置已重载，点击“启动服务”继续。"); }));
  on("#cc-config-read", () => perform("正在读取配置…", async () => { await loadConfiguration(); notice("已读取 brain.json。"); }));
  on("#cc-config-save", () => perform("正在保存配置…", async () => {
    const raw = $<HTMLTextAreaElement>("#cc-config-editor").value;
    if (raw.length > 200_000) throw new Error("配置内容过大。");
    const parsed: unknown = JSON.parse(raw);
    if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error("配置应为 JSON 对象。");
    await stopEverything(false); await request("config.save", parsed); await request("service.reload");
    resetConversationView(); await loadConfiguration(); await refreshFamily(); notice("配置已保存并重载。点击“启动服务”继续。");
  }));
  on("#cc-models-status", () => perform("正在读取本地模型状态…", async () => { $("#cc-models-json").textContent = pretty(await request("models.status")); notice("已读取本地模型状态；worker 就绪不代表所有模型都已安装。"); }));
  on("#cc-models-unload", () => perform("正在卸载本地模型…", async () => { await stopEverything(false); await request("models.unload"); $("#cc-models-json").textContent = "本地模型进程已退出；下次明确请求时按需加载。"; notice("已卸载。"); }));
  for (const tab of Array.from(container.querySelectorAll<HTMLButtonElement>(".cc-tabs button"))) tab.addEventListener("click", () => activatePanel(tab.dataset.panel ?? "talk"));
  $<HTMLFormElement>("#cc-chat-form").addEventListener("submit", (event) => { event.preventDefault(); void sendTurn(); });
  input.addEventListener("input", updateControls);
  input.addEventListener("keydown", (event) => { if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) { event.preventDefault(); void sendTurn(); } });
  profileSelect.addEventListener("change", () => { void changeProfile(profileSelect.value || null); });
  on("#cc-new-conversation", async () => { await stopEverything(false); resetConversationView(); notice("已开始新对话。旧对话可在“服务与设置”中一起清理。"); });
  on("#cc-forget-conversation", () => perform("正在忘记这段对话…", async () => {
    if (!window.confirm("忘记当前这段对话及其 OpenCode 会话？单独保存的记忆会保留。")) { notice(""); return; }
    const id = conversationId; await stopEverything(false); await request("conversation.forget", { conversation_id: id }); resetConversationView(); notice("这段对话已忘记。");
  }));
  on("#cc-forget-all", () => perform("正在忘记所有对话…", async () => {
    if (!window.confirm("清理全部本地 conversation 及对应 OpenCode 会话？单独保存的记忆与档案会保留。")) { notice(""); return; }
    await stopEverything(false); await request("conversation.forget_all"); resetConversationView(); notice("所有对话已忘记。");
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
    const generation = mediaGeneration;
    const result = await request("identity.enroll", { profile_id: selectedProfile, kind: identityKind.value,
      samples: samples.map((sample) => sample.base64), guardian_confirmed: true, replace_existing: $<HTMLInputElement>("#cc-enroll-replace").checked });
    if (generation !== mediaGeneration) return;
    clearSamples(); await refreshFamily(); notice(`登记已保存，共 ${number(object(result).sample_count)} 份样本。识别仍只提供候选。`);
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
    turnGeneration += 1; mediaGeneration += 1; recorder.cancel(); camera.close();
    stopPlayback(); clearReplayClips(); clearToken(); samples = []; wavDraft = null;
  }
  const onPageHide = () => { localCleanup(); void request("assistant.cancel").catch(() => {}); };
  window.addEventListener("pagehide", onPageHide);
  // Status is metadata-only. Never poll model inference or microphone/camera.
  const statusTimer = window.setInterval(() => { if (visible && !turnBusy && !operationBusy) void refreshStatus(); }, 15_000);
  const permissionTimer = window.setInterval(() => { if (visible && (turnBusy || serviceReady)) void refreshPermissions(); }, 3_000);
  if (!options.mock) {
    void listen<unknown>("brain:event", (event) => {
      const data = object(event.payload); const kind = string(data.kind);
      if (kind.includes("permission")) void refreshPermissions();
      if (["service.started", "service.stopped", "harness_started", "harness_stopped"].includes(kind)) void refreshStatus();
      if (kind === "service.stopped") { cancelCapture(); closeCamera(); stopPlayback(); phase("idle", "服务已停止。"); }
      const eventTurn = string(data.turn_id);
      if (!eventTurn || eventTurn !== activeTurn) return;
      if (kind === "turn.transcribing") phase("thinking", "先在 Mac 上听懂这句话…");
      else if (kind === "turn.thinking") phase("thinking");
      else if (kind === "turn.synthesizing") phase("thinking", "正在用本地声音准备回复…");
      else if (kind === "harness_permission_required") phase("thinking", "有一项操作需要家长确认。");
      else if (kind === "turn.cancelled") { stopPlayback(); phase("idle", "这一次已经停止。"); }
    }).then((stop) => { if (disposed) stop(); else unsubscribe = stop; }).catch((error) => notice(errorText(error), true));
    void Promise.allSettled([refreshStatus(), loadConfiguration(), refreshFamily()]).then((results) => {
      if (disposed) return;
      for (const result of results) if (result.status === "rejected") { notice(errorText(result.reason), true); break; }
    });
  } else { $("#cc-service-pill").textContent = "预览 · 未连接服务"; notice("这是旧版界面的预览模式；没有调用小呆服务，也没有打开麦克风或摄像头。"); }
  renderWelcome(); renderSamples(); resetMemoryForm(); updateControls();

  return {
    setVisible(next: boolean): void {
      visible = next;
      if (!visible) { cancelCapture(); closeCamera(); stopPlayback(); clearReplayClips(); clearToken(); clearSamples(); wavDraft = null; renderDraft(); }
      else { void refreshStatus(); void refreshPermissions(); }
    },
    dispose(): void {
      if (disposed) return;
      disposed = true; localCleanup(); unsubscribe?.();
      window.clearInterval(statusTimer); window.clearInterval(permissionTimer);
      window.removeEventListener("pagehide", onPageHide);
    },
  };
}
