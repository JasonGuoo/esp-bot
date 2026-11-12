import { invoke } from "@tauri-apps/api/core";
import { FACE_PREVIEWS } from "./face-catalog.generated";
import "./usb-debug.css";

type Frame = { width: number; height: number; sequence: number; device_ms: number; received_ms: number; data_url: string };
type Event = { id: number; received_ms: number; device_ms: number | null; kind: string; detail: string };
type State = { requested: boolean; connected: boolean; preview: boolean; camera_requested: boolean; port: string | null; error: string | null;
  camera_orientation: number | null;
  screen: Frame | null; camera: Frame | null; events: Event[]; invalid_packets: number; received_frames: number; lcd_underruns: number };

export function mountUsbDebug(parent: HTMLElement) {
  const root = document.createElement("section");
  root.className = "cc-box usb-debug";
  root.innerHTML = `<div class="cc-section-title"><div><span class="cc-eyebrow">USB 开发调试</span><h2>画面与设备事件</h2></div><span class="cc-tag" data-usb="state">未连接</span></div>
    <p class="cc-muted">并排查看小呆屏幕和相机检测输入。预览仅经本机 USB，默认保存在内存；连接串口可能使开发板重启。</p>
    <div class="usb-toolbar"><select data-usb="ports" aria-label="USB 串口"></select><button class="cc-button quiet" data-usb="refresh">刷新串口</button><button class="cc-button" data-usb="connect">连接 USB</button><button class="cc-button quiet" data-usb="disconnect" disabled>断开</button></div>
    <details class="usb-wifi"><summary>通过 USB 设置机器人 Wi-Fi</summary>
      <p class="cc-caption">先连接 USB，再填写与 Mac 可互相访问的 2.4 GHz 网络。密码不会保存在 Mac 控制台；当前开发板尚未启用存储加密，请保护好设备。</p>
      <form class="cc-form" data-usb="wifi-form" autocomplete="off">
        <div class="cc-two-fields"><label>网络名称<input data-usb="wifi-ssid" aria-label="Wi-Fi 网络名称" autocomplete="off" required></label>
        <label>网络密码<input data-usb="wifi-password" aria-label="Wi-Fi 网络密码" type="password" autocomplete="new-password" maxlength="64" placeholder="开放网络留空"></label></div>
        <button class="cc-button" type="submit" data-usb="wifi-submit" disabled>保存到机器人</button>
        <p class="cc-caption" data-usb="wifi-status" role="status">等待 USB 连接。</p>
      </form></details>
    <div class="usb-toolbar"><button class="cc-button" data-usb="wake" disabled>手动唤醒小呆</button><button class="cc-button quiet" data-usb="stop" disabled>停止本次互动</button><button class="cc-button quiet" data-usb="preview" disabled>暂停画面</button><button class="cc-button quiet" data-usb="camera-toggle" disabled>开启相机调试保活</button><span data-usb="metrics" class="cc-caption"></span></div>
    <p class="cc-caption">新版固件在白天空闲时会在设备本地检测附近人脸；夜间休息时停止。调试保活只是额外的相机请求，关闭它或断开 USB 不会关闭白天的本地检测。预览画面仅经本机 USB 传输。</p>
    <div class="usb-toolbar"><span class="cc-caption">独立眼睛测试：</span><button class="cc-button quiet" data-usb="eye-left" disabled>向左</button><button class="cc-button quiet" data-usb="eye-center" disabled>居中</button><button class="cc-button quiet" data-usb="eye-right" disabled>向右</button><span class="cc-caption">手动测试 10 秒，暂时覆盖自动注视，不唤醒对话。</span></div>
    <div class="usb-toolbar"><span class="cc-caption">表情预览：</span><select data-usb="face-choice" aria-label="选择表情">${FACE_PREVIEWS.map(e => `<option value="${e.id}">${e.name}</option>`).join("")}</select><button class="cc-button quiet" data-usb="face-show" disabled>预览表情</button><span class="cc-caption">手动显示 8 秒，之后恢复自动行为；水汪汪只用于特别开心。</span></div>
    <div class="usb-toolbar"><span class="cc-caption">摄像头方向：</span><select data-usb="orientation" aria-label="摄像头方向"><option value="0">原始方向</option><option value="1">左右镜像</option><option value="2">上下翻转</option><option value="3">左右及上下翻转</option></select><button class="cc-button quiet" data-usb="orientation-apply" disabled>保存并应用</button><span class="cc-caption" data-usb="orientation-status">等待设备报告方向</span></div>
    <p class="cc-caption">校正同时作用于相机预览和人脸检测，并保存在开发板；切换时相机会短暂重启。若预览没有恢复，请查看设备事件。</p>
    <details><summary>显示诊断</summary><div class="usb-toolbar"><button class="cc-button quiet" data-usb="screen-frame" disabled>使用刷新缓冲</button><button class="cc-button quiet" data-usb="screen-snap" disabled>对照：重新渲染截图</button><span class="cc-caption">仅用于本次 USB 调试；两种方式都不能直接看到实体 LCD 的光学错位。</span></div></details>
    <p data-usb="notice" class="cc-caption" role="status">连接后可查看屏幕和设备正在采集的相机画面。</p>
    <p data-usb="frame-skew" class="cc-caption">两路画面分别采集，等待时间信息。</p>
    <div class="usb-previews">
      <figure><figcaption><strong>Bot 屏幕</strong><span>界面渲染截图，不反映实体 LCD 故障</span></figcaption><div class="usb-image"><img data-usb="screen" alt="小呆屏幕的设备渲染帧" hidden><span data-usb="screen-empty">等待屏幕画面</span><span class="usb-stale" data-usb="screen-stale" hidden>画面未更新，请检查 USB 连接</span></div><div class="usb-frame-meta"><small data-usb="screen-time">尚无画面</small><button class="cc-button quiet" data-usb="export-screen" disabled>导出当前帧</button></div></figure>
      <figure><figcaption><strong>摄像头</strong><span>绿框为检测到的人脸，不一定正在跟随</span></figcaption><div class="usb-image"><img data-usb="camera" alt="小呆摄像头的人脸检测输入" hidden><span data-usb="camera-empty">等待相机画面；请检查 USB 预览和设备状态</span><span class="usb-stale" data-usb="camera-stale" hidden>画面未更新：相机可能已休眠或处于夜间模式</span></div><div class="usb-frame-meta"><small data-usb="camera-time">尚无画面</small><button class="cc-button quiet" data-usb="export-camera" disabled>导出当前帧</button></div></figure>
    </div>
    <div class="usb-event-heading"><h3>Events</h3><input data-usb="filter" placeholder="筛选 touch、vision、focus 或事件名" aria-label="筛选设备事件"><button class="cc-button quiet" data-usb="export-events">导出事件</button><button class="cc-button quiet" data-usb="clear">清空显示</button></div>
    <div class="usb-events" data-usb="events" aria-label="设备调试事件"></div>`;
  parent.prepend(root);
  const $ = <T extends HTMLElement = HTMLElement>(name: string) => root.querySelector<T>(`[data-usb="${name}"]`)!;
  const request = <T>(method: string, payload: unknown = {}) => invoke<T>("usb_debug_request", { method, payload });
  let disposed = false, busy = false, after = 0, wifiSentAt = 0;
  let state: State | null = null, wantedPreview = true;
  let lastOrientation: number | null = null;
  let events: Event[] = [], lastPreviewRequest = "";
  const frames: Record<string, string> = {};
  function notice(message: string) { $("notice").textContent = message; }
  async function action(work: () => Promise<unknown>) {
    try { await work(); } catch (error) { notice(String(error)); }
  }
  async function refreshPorts() {
    const selected = $<HTMLSelectElement>("ports").value;
    const ports = await request<string[]>("ports");
    const select = $<HTMLSelectElement>("ports"); select.replaceChildren();
    for (const port of ports) { const option = document.createElement("option"); option.value = port; option.textContent = port; select.append(option); }
    if (ports.includes(selected)) select.value = selected;
    if (!ports.length) notice("未发现 USB 串口，请检查数据线和开发板连接。");
  }
  function showFrame(source: "screen" | "camera", frame: Frame | null) {
    const image = $<HTMLImageElement>(source);
    const available = !!frame && !!state?.preview && !!state?.connected;
    image.hidden = !available; $(`${source}-empty`).hidden = available;
    $(`${source}-stale`).hidden = true;
    $<HTMLButtonElement>(`export-${source}`).disabled = !available;
    if (!available) { image.removeAttribute("src"); delete frames[source]; $(`${source}-time`).textContent = "尚无画面"; return; }
    const key = `${frame.sequence}:${frame.received_ms}`;
    if (frames[source] !== key) { image.src = frame.data_url; frames[source] = key; }
    const age = Math.max(0, (Date.now() - frame.received_ms) / 1000);
    image.classList.toggle("stale", age > 8);
    $(`${source}-stale`).hidden = age <= 8;
    $(`${source}-time`).textContent = `${frame.width}×${frame.height} · 设备 ${frame.device_ms} ms · ${age.toFixed(1)} 秒前${age > 8 ? "（最后一帧）" : ""}`;
  }
  function renderEvents() {
    const filter = $<HTMLInputElement>("filter").value.trim().toLowerCase();
    const list = $("events"); const nearBottom = list.scrollTop + list.clientHeight >= list.scrollHeight - 48;
    list.replaceChildren();
    for (const event of events.filter(event => `${event.kind} ${event.detail}`.toLowerCase().includes(filter)).slice(-150)) {
      const row = document.createElement("div"); row.className = "usb-event";
      const time = document.createElement("time"); time.textContent = event.device_ms === null ? new Date(event.received_ms).toLocaleTimeString() : `${event.device_ms} ms`;
      const kind = document.createElement("strong"); kind.textContent = event.kind;
      const detail = document.createElement("code"); detail.textContent = event.detail;
      row.append(time, kind, detail); list.append(row);
    }
    if (nearBottom) list.scrollTop = list.scrollHeight;
  }
  async function syncPreview() {
    if (!state?.connected) { lastPreviewRequest = ""; return; }
    // Switching between desktop windows must not stop an explicitly requested
    // debug session. Hiding the actual device page still stops preview.
    const desired = wantedPreview && !parent.hidden;
    const key = `${state.port}:${desired}`;
    if (lastPreviewRequest === key) return;
    lastPreviewRequest = key;
    try { await request("command", { command: desired ? "preview on" : "preview off" }); }
    catch { lastPreviewRequest = ""; }
  }
  async function poll() {
    if (disposed || busy) return;
    busy = true;
    try {
      state = await request<State>("state", { after });
      $("state").textContent = state.connected ? "USB 已连接" : state.requested ? "等待开发板" : "未连接";
      for (const name of ["wake", "stop", "preview", "eye-left", "eye-center", "eye-right", "screen-frame", "screen-snap"]) $<HTMLButtonElement>(name).disabled = !state.connected;
      $<HTMLButtonElement>("face-show").disabled = !state.connected || !state.preview;
      $<HTMLButtonElement>("orientation-apply").disabled = !state.connected || !state.preview || state.camera_orientation === null;
      if (state.camera_orientation !== lastOrientation) {
        lastOrientation = state.camera_orientation;
        if (lastOrientation !== null) $<HTMLSelectElement>("orientation").value = String(lastOrientation);
      }
      $("orientation-status").textContent = state.camera_orientation === null ? "等待设备报告方向" :
        `设备选择：${["原始方向", "左右镜像", "上下翻转", "左右及上下翻转"][state.camera_orientation]}`;
      // The user must be able to cancel reconnect and any remembered camera
      // request while the device is physically unplugged.
      $<HTMLButtonElement>("disconnect").disabled = !state.requested;
      $<HTMLButtonElement>("camera-toggle").disabled = !state.connected || !state.preview;
      $("camera-toggle").textContent = state.camera_requested ? "关闭相机调试保活" : "开启相机调试保活";
      $("camera-toggle").setAttribute("aria-pressed", String(state.camera_requested));
      $<HTMLButtonElement>("connect").disabled = state.requested;
      $<HTMLButtonElement>("wifi-submit").disabled = !state.connected;
      if (state.connected && $("wifi-status").textContent === "等待 USB 连接。")
        $("wifi-status").textContent = "USB 已连接，请填写网络信息。";
      if (wifiSentAt && Date.now() - wifiSentAt > 20000) {
        $("wifi-status").textContent = "尚未收到开发板确认，请检查 USB 连接后重试。";
        wifiSentAt = 0;
      }
      $("preview").textContent = wantedPreview ? "暂停画面" : "恢复画面";
      $("metrics").textContent = `${state.received_frames} 帧 · ${state.invalid_packets} 个校验/同步异常 · 驱动欠载日志 ${state.lcd_underruns} 次`;
      if (state.error) notice(state.error);
      else if (state.connected) notice("USB 画面和事件仅在本机处理。切换窗口会保持预览；退出调试页会暂停画面。");
      showFrame("screen", state.screen); showFrame("camera", state.camera);
      if (state.connected && state.preview && state.screen && state.camera) {
        // Signed modular difference also works across the device's 32-bit clock wrap.
        const delta = (state.screen.device_ms - state.camera.device_ms) | 0;
        const seconds = Math.abs(delta) / 1000;
        $("frame-skew").textContent = seconds > 300 ? "两路画面分别采集，时间相差较大，请等待更新。" :
          seconds < 0.1 ? "两路画面分别采集，采集间隔不足 0.1 秒。" :
          `两路画面分别采集；${delta < 0 ? "屏幕" : "摄像头"}采集时间较早 ${seconds.toFixed(1)} 秒。`;
      } else $("frame-skew").textContent = "两路画面分别采集，等待时间信息。";
      if (state.events.length) {
        for (const event of state.events) {
          if (["wifi.setup", "wifi.saved", "wifi.failed"].includes(event.kind)) {
            $("wifi-status").textContent = event.detail;
            if (event.kind !== "wifi.setup" || event.detail.includes("失败")) wifiSentAt = 0;
          }
          if (event.kind === "serial.log" && event.detail.includes("Wi-Fi address acquired"))
            $("wifi-status").textContent = "机器人已连接 Wi-Fi，正在查找 Server。";
        }
        events.push(...state.events); events = events.slice(-300); after = state.events.at(-1)!.id; renderEvents();
      }
      await syncPreview();
    } catch (error) { notice(String(error)); }
    finally { busy = false; }
  }
  $("refresh").addEventListener("click", () => { void action(refreshPorts); });
  $("connect").addEventListener("click", () => { void action(async () => { after = 0; events = []; wantedPreview = true; lastPreviewRequest = ""; await request("connect", { port: $<HTMLSelectElement>("ports").value }); await poll(); }); });
  $("disconnect").addEventListener("click", () => { void action(async () => { await request("disconnect"); await poll(); }); });
  $("wifi-form").addEventListener("submit", event => {
    event.preventDefault();
    const ssid = $<HTMLInputElement>("wifi-ssid").value;
    const passwordField = $<HTMLInputElement>("wifi-password");
    const password = passwordField.value;
    if (!ssid || new TextEncoder().encode(ssid).length > 32) {
      $("wifi-status").textContent = "网络名称须为 1–32 字节。"; return;
    }
    $("wifi-status").textContent = "正在发送 Wi-Fi 配置…";
    wifiSentAt = Date.now();
    const pending = request("wifi_provision", { ssid, password });
    passwordField.value = "";
    void pending.catch(error => { wifiSentAt = 0; $("wifi-status").textContent = String(error); });
  });
  for (const name of ["wake", "stop"]) $(name).addEventListener("click", () => { void action(() => request("command", { command: name })); });
  for (const direction of ["left", "center", "right"]) $(`eye-${direction}`).addEventListener("click", () => {
    void action(() => request("command", { command: `eye ${direction}` }));
  });
  $("face-show").addEventListener("click", () => {
    void action(() => request("command", { command: `face ${$<HTMLSelectElement>("face-choice").value}` }));
  });
  $("orientation-apply").addEventListener("click", () => {
    void action(() => request("command", { command: `camera orient ${$<HTMLSelectElement>("orientation").value}` }));
  });
  for (const mode of ["frame", "snap"]) $(`screen-${mode}`).addEventListener("click", () => {
    void action(() => request("command", { command: `screen ${mode}` }));
  });
  $("preview").addEventListener("click", () => { wantedPreview = !wantedPreview; void syncPreview(); });
  $("camera-toggle").addEventListener("click", () => { void action(() => request("command", {
    command: state?.camera_requested ? "camera off" : "camera on",
  })); });
  for (const source of ["screen", "camera"]) $(`export-${source}`).addEventListener("click", () => { void action(async () => {
    const result = await request<{ path: string }>("export", { source }); notice(`已导出当前帧：${result.path}`);
  }); });
  $("clear").addEventListener("click", () => { events = []; renderEvents(); });
  $("export-events").addEventListener("click", () => { void action(async () => {
    const result = await request<{ path: string }>("export_events"); notice(`已导出事件及最近崩溃记录：${result.path}`);
  }); });
  $("filter").addEventListener("input", renderEvents);
  const observer = new MutationObserver(() => { void syncPreview(); }); observer.observe(parent, { attributes: true, attributeFilter: ["hidden"] });
  const timer = window.setInterval(() => { void poll(); }, 750);
  void action(refreshPorts); void poll();
  return {
    dispose() { disposed = true; window.clearInterval(timer); observer.disconnect(); void request("disconnect").catch(() => {}); },
  };
}
