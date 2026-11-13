type Data = Record<string, unknown>;
export type DeviceDisplayReport = "rendered" | "unavailable" | null;

export function deviceDeliveryText(text: DeviceDisplayReport, image: DeviceDisplayReport): string {
  if (image === "rendered") {
    if (text === "rendered") return "文字和图片已绘制到设备屏幕";
    if (text === "unavailable") return "图片已绘制，文字暂时无法显示";
    return "图片已绘制到设备屏幕";
  }
  if (image === "unavailable") {
    if (text === "rendered") return "文字已绘制，图片暂时无法显示";
    if (text === "unavailable") return "文字和图片暂时无法显示";
    return "图片暂时无法显示";
  }
  if (text === "rendered") return "文字已绘制到设备屏幕";
  if (text === "unavailable") return "文字暂时无法显示";
  return "Server 已完成，设备显示状态待确认";
}

export function deviceReplyMetrics(payload: Data): string {
  const parts: string[] = [];
  const { processing_ms: duration, tts_calls: calls, reply_pcm_bytes: bytes } = payload;
  if (typeof duration === "number" && Number.isFinite(duration) && duration >= 0)
    parts.push(`Mac 处理 ${(duration / 1000).toFixed(1)} 秒`);
  if (typeof calls === "number" && Number.isSafeInteger(calls) && calls >= 0)
    parts.push(`语音合成 ${calls} 次`);
  if (typeof bytes === "number" && Number.isSafeInteger(bytes) && bytes >= 0)
    parts.push(`回复音频 ${bytes.toLocaleString("zh-CN")} 字节`);
  return parts.join(" · ");
}
const object = (value: unknown): Data => value !== null && typeof value === "object" && !Array.isArray(value) ? value as Data : {};
const text = (value: unknown, fallback = ""): string => typeof value === "string" ? value : fallback;
const list = (value: unknown): unknown[] => Array.isArray(value) ? value : [];
const node = <K extends keyof HTMLElementTagNameMap>(tag: K, className: string, content = ""): HTMLElementTagNameMap[K] => {
  const result = document.createElement(tag); result.className = className; result.textContent = content; return result;
};
const find = (root: HTMLElement, selector: string): HTMLElement => {
  const result = root.querySelector<HTMLElement>(selector);
  if (!result) throw new Error(`Missing console element ${selector}`);
  return result;
};

const TIMING_LABELS: Record<string, string> = {
  memory_register: "会话登记", asr: "语音识别", speaker_identification: "声纹匹配",
  memory_enqueue: "记忆排队", memory_context: "记忆检索", harness: "OpenCode 回复",
  image_prepare: "图片准备", tts: "语音合成", history: "会话保存",
};
const TIMING_SKIP_LABELS: Record<string, string> = {
  text_input: "文字输入", no_enrolled_speakers: "尚未登记声纹", audio_under_3_seconds: "录音不足 3 秒",
  enrollment_lookup_failed: "登记信息读取失败", no_device_image: "本轮无需设备图片", output_disabled: "未开启语音输出",
};

export function turnStageMetric(value: unknown): string {
  const stage = object(value);
  const label = TIMING_LABELS[text(stage.stage)];
  const duration = stage.elapsed_ms;
  if (!label || typeof duration !== "number" || !Number.isFinite(duration) || duration < 0) return "";
  const status = stage.status === "failed" ? " · 失败" : stage.status === "interrupted" ? " · 已中断" : "";
  return `${label} · ${Math.round(duration).toLocaleString("zh-CN")} ms${status}`;
}

export function harnessTimingMetric(value: unknown): string {
  const event = object(value);
  const route = text(event.route_id, "当前路线");
  const duration = event.kind === "harness_route_selected" ? event.prepare_ms : event.elapsed_ms;
  const elapsed = typeof duration === "number" && Number.isFinite(duration) && duration >= 0
    ? ` · ${(duration / 1000).toFixed(1)} 秒` : "";
  switch (event.kind) {
    case "harness_route_selected": return `${route} · 已选择，准备完成${elapsed}`;
    case "harness_route_failed": return `${route} · ${text(event.code, "调用未完成")}${elapsed} · ${event.safe_to_fallback === true ? "可继续候补路线" : "本轮不自动重做"}`;
    case "harness_turn_completed": return `${route} · 完整回复已接收${elapsed}`;
    case "harness_first_text": return `${route} · 收到首段模型文字${elapsed}（等待完整回复）`;
    case "harness_upstream_retry": return `上游正在重试 · ${text(event.code, "原因待确认")}`;
    default: return "";
  }
}

/** Parent-console diagnostics; never included in the child's device scene. */
export function renderTurnTimings(value: unknown): HTMLElement | null {
  const timing = object(value);
  const stages = object(timing.stages_ms), skipped = object(timing.skipped);
  if (!Object.keys(stages).length && !Object.keys(skipped).length) return null;
  const details = node("details", "cc-reply-timings");
  const total = typeof timing.total_ms === "number" && Number.isFinite(timing.total_ms) && timing.total_ms >= 0
    ? ` · ${(timing.total_ms / 1000).toFixed(1)} 秒` : "";
  details.append(node("summary", "cc-caption", `查看 Mac 分段耗时${total}`));
  for (const [stage, label] of Object.entries(TIMING_LABELS)) {
    const metric = turnStageMetric({ stage, elapsed_ms: stages[stage] });
    if (metric) details.append(node("p", "cc-caption", metric));
    else if (TIMING_SKIP_LABELS[text(skipped[stage])])
      details.append(node("p", "cc-caption", `${label} · 已跳过：${TIMING_SKIP_LABELS[text(skipped[stage])]}`));
  }
  details.append(node("p", "cc-caption", "OpenCode 耗时包含模型、工具、路线回退及人工确认等待；不包含设备判停、上传和屏幕绘制。"));
  return details;
}

export function renderCapabilities(root: HTMLElement, value?: unknown): void {
  const report = object(value);
  const models = object(object(report.models).models);
  const host = find(root, "#cc-model-capabilities"); host.replaceChildren();
  for (const [kind, label, description] of [
    ["asr", "语音转文字", "本地识别主动发送的录音"],
    ["tts", "语音朗读", "使用已安装的 macOS 声音"],
    ["face", "人脸识别", "本机登记与寻找成员候选"],
    ["speaker", "声纹识别", "本机提取声音特征"],
  ]) {
    const model = object(models[kind]);
    const card = node("article", "cc-capability");
    let status = "尚未检查"; let tone = "";
    if (value !== undefined && report.ready !== true) { status = "检查未完成"; tone = "unavailable"; }
    else if (model.available === true) { status = "环境就绪"; tone = "good"; }
    else if (model.supported === false) { status = "系统不支持"; tone = "unavailable"; }
    else if (model.installed === false) { status = "模型未安装"; tone = "warning"; }
    else if (list(model.missing_dependencies).length) { status = "缺少依赖"; tone = "warning"; }
    card.append(node("h3", "", label), node("span", `cc-tag ${tone}`, status), node("p", "", description));
    if (kind === "asr" && model.backend === "rma_qwen3_asr_decode_only") card.append(node("p", "", "Qwen3-ASR · RMA 优化引擎"));
    if (model.loaded === true) card.append(node("p", "", "模型已加载"));
    if (list(model.missing_dependencies).length) card.append(node("p", "", `待安装：${list(model.missing_dependencies).map(item => text(item)).join("、")}`));
    if (kind === "tts" && model.available === true) card.append(node("p", "", `声音：${text(model.default_voice, "系统默认")}，尚未试听`));
    host.append(card);
  }
}

export function renderDeviceStatus(root: HTMLElement, status: Data): void {
  const diagnostic = object(status.device_diagnostics);
  const bots = list(diagnostic.bots);
  find(root, "#cc-device-summary").textContent = diagnostic.running === true
    ? bots.length ? `${bots.length} 台设备诊断在线` : "等待机器人连接"
    : "诊断通道未就绪";
  find(root, "#cc-device-detail").textContent = diagnostic.running === true
    ? `LAN 发现服务运行中 · 端口 ${diagnostic.port ?? "待读取"}。这里只交换设备状态，不传输私密媒体。`
    : text(diagnostic.error, "正在读取设备发现服务状态。");
  const host = find(root, "#cc-device-list"); host.replaceChildren();
  for (const item of bots) {
    const bot = object(item);
    const row = node("div", "cc-manage-row");
    const meta = node("div", "cc-manage-meta");
    meta.append(node("strong", "", text(bot.name, "机器人")), node("code", "cc-small-code", text(bot.device_id)), node("span", "cc-caption", `来自 ${text(bot.peer, "本地网络")} · 诊断连接`));
    row.append(meta, node("span", "cc-tag good", "在线")); host.append(row);
  }
  if (diagnostic.server_id) host.append(node("p", "cc-caption", `服务器标识：${text(diagnostic.server_id)}`));
  const gateway = object(status.gateway);
  const secureDevices = list(gateway.devices);
  find(root, "#cc-gateway-summary").textContent = gateway.running === true
    ? secureDevices.length ? `${secureDevices.length} 台设备已认证连接` : "WSS 网关已启动，等待设备"
    : status.gateway_enabled === true ? "已启用，等待服务启动" : "WSS 网关未启用";
  find(root, "#cc-gateway-detail").textContent = gateway.running === true
    ? `${text(gateway.bind)}${text(gateway.path)} · ${gateway.bonjour ? "Bonjour 已公布" : "Bonjour 未公布"} · 控制连接 TLS · 语音局域网明文 · 协议 v3${secureDevices.length ? ` · ${secureDevices.map((v) => text(v)).join("、")}` : ""}`
    : "在下方配置证书与监听地址，并通过 USB 配置设备。";
}

export function renderRuntimeMetrics(root: HTMLElement, status: Data): void {
  const harness = object(status.harness);
  const host = find(root, "#cc-runtime-metrics"); host.replaceChildren();
  for (const [field, label] of [["active_turns", "进行中的任务"], ["pending_permissions", "待确认的操作"], ["retained_conversations", "保留的对话上下文"]]) {
    const card = node("div", "cc-runtime-metric");
    card.append(node("strong", "", typeof harness[field] === "number" ? String(harness[field]) : "未读取"), node("span", "", label)); host.append(card);
  }
}

export function renderActivity(root: HTMLElement, events: { label: string; at: number }[]): void {
  const host = find(root, "#cc-activity-list"); host.replaceChildren();
  if (!events.length) host.append(node("p", "cc-muted", "本次运行还没有记录。"));
  for (const event of events.slice().reverse()) {
    const row = node("div", "cc-activity-row");
    const time = node("time", "", new Date(event.at).toLocaleTimeString("zh-CN", { hour12: false })); time.dateTime = new Date(event.at).toISOString();
    row.append(time, node("span", "", event.label)); host.append(row);
  }
}
