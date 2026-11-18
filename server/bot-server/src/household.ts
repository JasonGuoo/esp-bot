import "./household.css";

type RecordValue = Record<string, unknown>;
type Profile = { id: string; display_name: string; is_child: boolean; merged_into?: string | null };
type Request = <T = unknown>(method: string, payload?: unknown) => Promise<T>;
type Options = { request: Request; openVoiceGroup(id: number): void; notice(text: string, error?: boolean): void; profiles(): Profile[]; onProfilesChange?(): Promise<void>; onPlaybackChange?(playing: boolean): void };
export type HouseholdController = {
  setPanel(panel: string, visible: boolean): void;
  refresh(): Promise<void>;
  showSource(id: string): Promise<void>;
  showTask(id: string): Promise<void>;
  showPresentation(id: string): Promise<void>;
  showMerge(): Promise<void>;
  stopMedia(): void;
  dispose(): void;
};
const obj = (value: unknown): RecordValue => value !== null && typeof value === "object" && !Array.isArray(value) ? value as RecordValue : {};
const arr = (value: unknown): unknown[] => Array.isArray(value) ? value : [];
const str = (value: unknown, fallback = ""): string => typeof value === "string" ? value : fallback;
const num = (value: unknown, fallback = 0): number => typeof value === "number" && Number.isFinite(value) ? value : fallback;
function node<K extends keyof HTMLElementTagNameMap>(tag: K, className = "", text = ""): HTMLElementTagNameMap[K] {
  const result = document.createElement(tag); result.className = className; result.textContent = text; return result;
}
const labels: Record<string, string> = {
  queued: "排队中", running: "进行中", awaiting_input: "等待补充", awaiting_parent: "等待确认", succeeded: "已完成",
  partial: "部分完成", failed: "失败", cancelled: "已取消", cancel_requested: "正在取消", unknown: "结果待核对",
  completed: "已生成", transcribed: "已转写", received: "已收到", blocked: "等待条件", obsolete: "已被新版本替代",
  accepted: "已接下任务", preparing: "准备中", searching: "查找资料", drawing: "绘制中", synthesizing: "准备语音",
  rendering: "生成画面", submitting: "提交打印", submitted: "已提交队列", recovery_required: "重启后待核对",
  event: "发生的事", topic: "聊过的话题", learning: "学习记录", fact: "长期记忆", organize: "整理记忆",
  graph_ingest: "更新关系索引", graph_delete: "清理关系索引", introduced: "刚接触", self_reported: "本人描述",
  correct_with_help: "提示后答对", correct_independently: "独立答对", retained_later: "隔天仍记得",
  rendered: "画面已显示", finished: "声音已播完", started: "已开始播放", not_run: "尚未执行", passed: "已通过",
  displayed: "画面已显示", prepared: "内容已准备", audio_started: "声音已开始", audio_completed: "声音已播完",
  matched: "找到候选", ambiguous: "候选有歧义", low_quality: "音频质量不足", no_enrollment: "尚未登记声纹",
  text: "手动文字", pending: "识别尚未返回", overlap: "多人重叠", unsupported: "暂不支持",
  "print.submit": "打印", "media.prepare": "准备媒体", "presentation.render": "制作图文", "agent.work": "后台资料任务",
};
const label = (value: unknown, fallback = "待更新"): string => labels[str(value)] ?? str(value, fallback);
const stamp = (value: unknown): string => num(value) > 0 ? new Date(num(value)).toLocaleString("zh-CN", { hour12: false }) : "时间未记录";
function safeUrl(value: unknown): string | null {
  try { const url = new URL(str(value)); return ["https:", "http:"].includes(url.protocol) && !url.username && !url.password ? url.href : null; }
  catch { return null; }
}
function sizeLabel(value: unknown): string { const bytes = num(value); return bytes >= 1024 * 1024 ? `${(bytes / 1024 / 1024).toFixed(1)} MB` : `${Math.ceil(bytes / 1024)} KB`; }
/** Re-read source-authorized chunks for every playback, including old replies. */
export async function readArtifactContent(request: Request, asset: RecordValue, current: () => boolean,
  progress?: (percent: number) => void, maxBytes = 64 * 1024 * 1024): Promise<ArrayBuffer | null> {
  const id = str(asset.artifact_id), total = num(asset.bytes), mime = str(asset.mime_type);
  if (!id || !Number.isSafeInteger(total) || total <= 0 || total > maxBytes) throw new Error("作品大小超过本次读取上限。");
  const buffer = new ArrayBuffer(total), content = new Uint8Array(buffer); let offset = 0;
  while (offset < total) {
    if (!current()) return null;
    const chunk = obj(await request("artifact.read", { artifact_id: id, offset, length: Math.min(262144, total - offset) }));
    if (!current()) return null;
    if (chunk.artifact_id !== id || num(chunk.offset, -1) !== offset || str(chunk.mime_type) !== mime) throw new Error("作品分块与当前读取不一致，已停止播放。");
    const binary = atob(str(chunk.data_base64)), count = binary.length;
    if (!count || count !== num(chunk.bytes) || count > total - offset || chunk.eof !== (offset + count === total)) throw new Error("作品分块长度或结束标志不正确。");
    for (let at = 0; at < count; at++) content[offset + at] = binary.charCodeAt(at);
    offset += count; progress?.(Math.round(offset / total * 100));
  }
  const digest = new Uint8Array(await crypto.subtle.digest("SHA-256", buffer));
  if (!current()) return null;
  const hex = Array.from(digest, value => value.toString(16).padStart(2, "0")).join("");
  if (hex !== str(asset.digest).toLowerCase()) throw new Error("作品完整性校验失败，已停止播放。");
  return buffer;
}
function problem(error: unknown): string {
  const value = String(error);
  if (value.includes("source_revision_conflict")) return "这段来源刚有新的识别或修订。请重新打开来源，对照最新内容后再保存；当前修改未覆盖别人操作。";
  if (value.includes("source_expired") || value.includes("source_not_retained")) return "原始来源已到期或未保留，不能再修改原文。";
  if (value.includes("artifact_missing_or_expired")) return "这份作品已删除或到期。";
  return value.slice(0, 600);
}
function scoreText(match: RecordValue): string {
  const values: string[] = [];
  const score = typeof match.best_score === "number" ? match.best_score : match.score;
  if (typeof score === "number" && Number.isFinite(score)) values.push(`相似度 ${score.toFixed(3)}`);
  if (typeof match.runner_up_score === "number") values.push(`次高 ${match.runner_up_score.toFixed(3)}`);
  if (typeof match.threshold === "number") values.push(`阈值 ${match.threshold.toFixed(3)}`);
  return values.length ? ` · ${values.join(" · ")}（不是正确率）` : "";
}

export function mountHousehold(household: HTMLElement, tasks: HTMLElement, options: Options): HouseholdController {
  const { request, notice } = options;
  let disposed = false, visible = true, panel = "", busy = false, generation = 0;
  let dailyId = "", sourceGeneration = 0, artifactGeneration = 0, taskGeneration = 0;
  let currentSource: RecordValue | null = null, currentTask: RecordValue | null = null;
  let settings: RecordValue = {}, timeline: RecordValue[] = [], taskRows: RecordValue[] = [];
  let timelineLimit = 60, settingsDirty = false;
  let report: RecordValue | null = null;
  const blobUrls = new Set<string>();
  const mediaElements = new Set<HTMLMediaElement>();
  function control(text: string, action: () => Promise<void> | void, className = "cc-button quiet"): HTMLButtonElement {
    const button = node("button", className, text); button.type = "button";
    button.addEventListener("click", async () => {
      if (button.disabled || disposed) return;
      button.disabled = true;
      if (button.nextElementSibling?.classList.contains("hh-action-error")) button.nextElementSibling.remove();
      try { await action(); } catch (error) {
        if (!disposed) {
          notice(problem(error), true);
          if (button.closest("dialog")) {
            const message = node("p", "hh-action-error", problem(error)); message.setAttribute("role", "alert"); button.after(message);
          }
        }
      }
      finally { if (!disposed && button.isConnected) button.disabled = false; }
    }); return button;
  }
  function field<T extends HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>(text: string, input: T): HTMLLabelElement {
    const row = node("label", "hh-field"); row.append(node("span", "", text), input); return row;
  }
  function check(text: string): { row: HTMLLabelElement; input: HTMLInputElement } {
    const input = node("input"); input.type = "checkbox"; const row = node("label", "cc-check"); row.append(input, document.createTextNode(text)); return { row, input };
  }
  function box(title: string, description = ""): HTMLElement {
    const section = node("section", "cc-box hh-section"); section.append(node("h2", "", title));
    if (description) section.append(node("p", "cc-muted", description)); return section;
  }
  function pills(...texts: string[]): HTMLElement { const result = node("div", "cc-memory-tags"); for (const text of texts.filter(Boolean)) result.append(node("span", "cc-tag", text)); return result; }
  function empty(host: HTMLElement, text: string): void { host.replaceChildren(node("p", "cc-muted", text)); }
  function name(id: unknown): string { return options.profiles().find(profile => profile.id === str(id))?.display_name ?? (str(id) ? `成员 ${str(id).slice(-6)}` : "未知说话人"); }
  function sourceLink(id: unknown, text = "查看原始对话"): HTMLButtonElement { return control(text, () => openSource(str(id))); }
  function links(host: HTMLElement, value: unknown): void {
    const record = obj(value); const urls = [record.url, record.source_url, record.origin_url, ...arr(record.sources).map(item => obj(item).url)];
    const unique = new Set(urls.map(safeUrl).filter((url): url is string => !!url));
    for (const url of unique) { const link = node("a", "hh-source-link", `来源 · ${new URL(url).hostname}`); link.href = url; link.target = "_blank"; link.rel = "noopener noreferrer"; host.append(link); }
  }
  function dialog(title: string): { modal: HTMLDialogElement; body: HTMLElement; heading: HTMLElement } {
    const modal = node("dialog", "cc-editor-dialog hh-dialog"); const heading = node("div", "cc-dialog-heading"); const body = node("div", "hh-dialog-body");
    heading.append(node("h2", "", title), control("关闭", () => modal.close())); modal.append(heading, body); (household.parentElement ?? household).append(modal); return { modal, body, heading };
  }
  const sourceDialog = dialog("对话来源与说话人");
  const artifactDialog = dialog("作品预览");
  sourceDialog.modal.addEventListener("close", () => { sourceGeneration++; currentSource = null; sourceDialog.body.replaceChildren(); });
  artifactDialog.modal.addEventListener("close", () => { stopMedia(); artifactDialog.body.replaceChildren(); });
  function stopMedia(): void {
    artifactGeneration++;
    for (const media of mediaElements) { media.pause(); media.removeAttribute("src"); media.load(); }
    mediaElements.clear(); for (const url of blobUrls) URL.revokeObjectURL(url); blobUrls.clear();
    options.onPlaybackChange?.(false);
  }
  function playbackChanged(): void { options.onPlaybackChange?.([...mediaElements].some(media => !media.paused && !media.ended)); }

  const tabs = node("div", "hh-tabs");
  const daySection = box("一家人的一天", "对话按家庭时区归入同一天。说话人是对话的参与者，人物关系和共同话题一起保留。");
  const memorySection = box("从对话中整理的记忆", "每条记忆都能回到原话。学习记录保留证据阶段，不用一次回答推断已经掌握。");
  const adminSection = box("记忆与归档设置", "修改或忘记来源会使旧派生记忆失效，并排队重建关系索引。");
  const sections = [daySection, memorySection, adminSection];
  for (const [index, text] of ["时间线", "家庭记忆", "后台与设置"].entries()) {
    const button = control(text, () => { sections.forEach((section, i) => { section.hidden = i !== index; }); for (const tab of Array.from(tabs.children)) tab.classList.toggle("selected", tab === button); });
    button.classList.add("hh-tab"); button.classList.toggle("selected", index === 0); tabs.append(button);
  }
  memorySection.hidden = adminSection.hidden = true;
  household.append(tabs, daySection, memorySection, adminSection);
  const daySelect = node("select"); daySelect.setAttribute("aria-label", "选择家庭对话日期");
  const search = node("input"); search.type = "search"; search.placeholder = "查找当天的人、问题或关键词"; search.setAttribute("aria-label", "筛选时间线");
  const dayControls = node("div", "hh-toolbar");
  dayControls.append(daySelect, search, control("刷新", refresh), control("忘记这一天", async () => {
    if (!dailyId || !window.confirm("忘记所选日期的全部对话来源及其派生记忆？关系索引的清理会在后台继续。")) return;
    await request("household.forget", { daily_conversation_id: dailyId }); dailyId = ""; currentSource = null; await refresh(); notice("所选日期的来源已忘记，索引清理已排队。");
  }, "cc-button danger"));
  const daySummary = node("p", "cc-caption"); const timelineHost = node("div", "hh-timeline");
  daySection.append(dayControls, daySummary, timelineHost);
  daySelect.addEventListener("change", () => {
    dailyId = daySelect.value; timelineLimit = 60;
    void Promise.allSettled([loadTimeline(), ...(sameDay.input.checked ? [loadMemories()] : [])]).then(results => {
      for (const result of results) if (result.status === "rejected") notice(problem(result.reason), true);
    });
  });
  search.addEventListener("input", () => { timelineLimit = 60; renderTimeline(); });

  const kindSelect = node("select"); kindSelect.setAttribute("aria-label", "记忆类型");
  for (const [value, text] of [["", "所有记忆"], ["event", "发生的事"], ["topic", "聊过的话题"], ["learning", "学习记录"], ["fact", "长期记忆"]]) { const option = node("option", "", text); option.value = value; kindSelect.append(option); }
  const sameDay = check("只看所选日期"); const memoryHost = node("div", "hh-memory-grid"); const memoryBar = node("div", "hh-toolbar");
  memoryBar.append(kindSelect, sameDay.row, control("刷新记忆", loadMemories)); memorySection.append(memoryBar, memoryHost);
  kindSelect.addEventListener("change", () => { void loadMemories().catch(error => notice(problem(error), true)); });
  sameDay.input.addEventListener("change", () => { void loadMemories().catch(error => notice(problem(error), true)); });

  const zone = node("input"); zone.placeholder = "例如 Asia/Shanghai"; zone.required = true;
  const retention = node("input"); retention.type = "number"; retention.min = "1"; retention.max = "365"; retention.required = true;
  const history = check("保留家庭对话原文"); const cloud = check("允许新家庭来源用于云端对话与整理");
  const settingsForm = node("form", "cc-form"); const settingsNote = node("p", "cc-caption");
  settingsForm.append(field("家庭时区（IANA）", zone), field("原文保留天数（1–365）", retention), history.row, cloud.row,
    node("p", "cc-caption", "关闭原文保留会清理现有原文及依赖内容；关闭云端使用会撤销已有来源的云端许可。重新开启只影响新来源。完整录音不归档。开启「谁在说话」后，可在那里试听短片段。"),
    control("保存设置", async () => {
      if (!settingsForm.reportValidity()) return;
      if (settings.history_enabled === true && !history.input.checked && !window.confirm("关闭归档并清理已保留的家庭对话原文？")) return;
      settings = obj(await request("household.settings.set", { timezone: zone.value.trim(), retention_secs: Number(retention.value) * 86400,
        history_enabled: history.input.checked, allow_cloud: cloud.input.checked })); settingsDirty = false; await refresh(); notice("家庭记忆设置已保存。");
    }, "cc-button primary"), settingsNote);
  settingsForm.addEventListener("submit", event => event.preventDefault()); settingsForm.addEventListener("input", () => { settingsDirty = true; });
  const jobsHost = node("div", "hh-job-list"); const jobsBar = node("div", "hh-toolbar");
  const graphHealth = node("p", "cc-caption", "关系索引运行状态尚未检查。");
  jobsBar.append(control("重试失败的整理", async () => { await request("household.retry"); await loadJobs(); notice("可重试的记忆工作已重新排队。"); }),
    control("重建关系索引", async () => { await request("household.graph.rebuild"); await loadJobs(); notice("关系索引重建已排队，原始对话仍以本机档案为准。"); }),
    control("检查关系索引状态", async () => {
      const health = obj(await request("household.graph.status"));
      const status = health.configured === true && health.models_not_probed === true
        ? "配置检查通过；图数据库连接与模型调用尚未验收。"
        : health.ready === true ? "关系索引服务已就绪" : `关系索引服务未就绪：${str(health.state, str(health.error, "暂不可用"))}`;
      graphHealth.textContent = `${new Date().toLocaleTimeString("zh-CN")} · ${status}`;
    }));
  adminSection.append(settingsForm, node("h3", "", "后台整理"), jobsBar, graphHealth, jobsHost);
  const merge = node("details", "hh-profile-merge"); merge.append(node("summary", "", "合并重复的家庭成员档案"),
    node("p", "cc-caption", "仅在两个档案确实是同一人时合并。来源档案保留为历史别名，原始识别候选和标注历史保留；关联记忆保留个人范围及原有云端许可。来源档案的识别模板停用，保留目标的模板，不混合训练。"));
  const mergeSource = node("select"), mergeTarget = node("select");
  function renderMergeOptions(): void {
    const before = [mergeSource.value, mergeTarget.value];
    for (const [index, select] of [mergeSource, mergeTarget].entries()) {
      select.replaceChildren(); const blank = node("option", "", index === 0 ? "选择重复的来源档案" : "选择要保留的目标档案"); blank.value = ""; select.append(blank);
      for (const person of options.profiles().filter(profile => !profile.merged_into)) { const choice = node("option", "", person.display_name); choice.value = person.id; select.append(choice); }
      select.value = before[index];
    }
  }
  const guardian = check("我是家长 / 本人，确认这两个档案指向同一个人");
  merge.append(field("合并来源（归档为历史别名）", mergeSource), field("保留目标", mergeTarget), guardian.row,
    control("刷新成员选择", async () => { await options.onProfilesChange?.(); renderMergeOptions(); }),
    control("确认合并这两个档案", async () => {
      const source = mergeSource.value, target = mergeTarget.value;
      if (!source || !target || source === target) throw new Error("请选择两个不同的档案，并确认来源与保留目标。");
      if (!guardian.input.checked) throw new Error("请先确认两个档案属于同一个人。");
      if (!window.confirm(`将“${name(source)}”合并到“${name(target)}”？来源会成为历史别名，其识别模板停用。`)) return;
      await request("profile.merge", { source_profile_id: source, target_profile_id: target, guardian_confirmed: true });
      guardian.input.checked = false; mergeSource.value = ""; mergeTarget.value = "";
      await options.onProfilesChange?.(); renderMergeOptions(); await refresh();
      notice("重复档案已合并，原始候选历史保留；请使用保留的目标档案。相关记忆正在重新整理。");
    }, "cc-button primary")); adminSection.append(merge);
  merge.addEventListener("toggle", () => { if (merge.open) renderMergeOptions(); });

  function renderTimeline(): void {
    const term = search.value.trim().toLocaleLowerCase();
    const filtered = timeline.filter(turn => `${str(turn.text)} ${str(turn.assistant_generated)} ${arr(turn.segments).map(segment => {
      const item = obj(segment); return name(item.annotation_set ? item.annotated_person_id : item.automatic_person_id);
    }).join(" ")}`.toLocaleLowerCase().includes(term));
    daySummary.textContent = `${daySelect.selectedOptions[0]?.textContent ?? "未选择日期"} · ${filtered.length} 轮${term ? "匹配" : "对话"} · 原文与实际呈现回执分别保存`;
    timelineHost.replaceChildren();
    for (const turn of filtered.slice(0, timelineLimit)) {
      const card = node("article", "hh-turn"); const people = arr(turn.segments).map(segment => {
        const value = obj(segment); return `${name(value.annotation_set ? value.annotated_person_id : value.automatic_person_id)}${value.annotation_set ? " · 已标注" : value.automatic_person_id ? " · 候选" : ""}`;
      });
      const head = node("div", "hh-turn-head"); head.append(node("time", "cc-caption", stamp(turn.occurred_at_ms)), pills(label(turn.status), turn.private_profile_id ? "保留的个人范围" : "家庭共享"));
      card.append(head, node("h3", "", [...new Set(people)].join(" / ") || "说话人待识别"), node("p", "hh-quote", str(turn.text) || (turn.source_expired ? "原文已到期" : "尚无可查看的转写")));
      if (turn.assistant_generated) card.append(node("p", "hh-answer", `小呆：${str(turn.assistant_generated)}`));
      const receipts = arr(turn.receipts); const caption = receipts.length ? receipts.map(item => { const value = obj(item); return receiptLabel(value); }).slice(-3).join(" · ") : "尚无设备呈现回执";
      card.append(node("p", "cc-caption", caption), sourceLink(turn.conversation_turn_id, "查看来源 / 标注说话人"));
      timelineHost.append(card);
    }
    if (!filtered.length) empty(timelineHost, dailyId ? "这一天还没有匹配的对话。保留设置开启后，新对话会显示在这里。" : "还没有家庭日记录。可以先与小呆聊一轮，再刷新。");
    if (filtered.length > timelineLimit) timelineHost.append(control("显示更多对话", () => { timelineLimit += 60; renderTimeline(); }));
  }
  function receiptLabel(value: RecordValue): string {
    const payload = obj(value.payload); const part = str(payload.part) === "image" ? "图片" : str(payload.part) === "text" ? "文字" : "语音 / 呈现";
    return `${part} ${presentationState(payload.status ?? payload.state ?? value.kind)}`;
  }
  function presentationState(value: unknown): string { return str(value) === "completed" ? "呈现完成" : label(value); }
  async function loadTimeline(): Promise<void> {
    if (!dailyId) { timeline = []; renderTimeline(); return; }
    const id = dailyId; const result = obj(await request("household.timeline", { daily_conversation_id: id }));
    if (disposed || id !== dailyId) return; timeline = arr(result.turns).map(obj); renderTimeline();
  }
  async function openSource(id: string): Promise<void> {
    if (!id) return; const version = ++sourceGeneration; empty(sourceDialog.body, "正在读取原始对话…"); if (!sourceDialog.modal.open) sourceDialog.modal.showModal();
    sourceDialog.heading.querySelector("h2")!.textContent = "对话来源与说话人";
    let value: RecordValue;
    try { value = obj(await request("household.source", { conversation_turn_id: id })); }
    catch (error) { if (!disposed && version === sourceGeneration) empty(sourceDialog.body, problem(error)); throw error; }
    if (disposed || version !== sourceGeneration || !sourceDialog.modal.open) return;
    currentSource = value; const body = sourceDialog.body; body.replaceChildren();
    body.append(pills(stamp(value.occurred_at_ms), `修订 ${num(value.source_revision)}`, value.source_expired ? "原文已到期" : "来源可查看"),
      node("p", "hh-quote", str(value.text) || "没有保留的原文"), node("p", "hh-answer", `小呆生成的回答：${str(value.assistant_generated, "未生成")}`),
      node("p", "cc-caption", "声纹候选供核对；标注只修正人物与来源的关联，不登记新的声纹模板，也不改变工具权限。"));
    for (const raw of arr(value.segments)) {
      const segment = obj(raw); const row = box("谁在说话"); const candidate = obj(segment.candidate); const match = obj(candidate.match ?? candidate);
      const span = num(segment.sample_rate) && segment.start_sample !== null ? `音频 ${(num(segment.start_sample) / num(segment.sample_rate)).toFixed(1)}–${(num(segment.end_sample) / num(segment.sample_rate)).toFixed(1)} 秒` : "整段输入";
      row.append(pills(span, segment.overlap === true ? "多人重叠" : "", label(segment.quality, "质量待判断")),
        node("p", "cc-caption", `自动候选：${name(segment.automatic_person_id)}${scoreText(match)}；人工标注：${segment.annotation_set ? name(segment.annotated_person_id) : "未标注"}`));
      if (str(match.reason) || str(candidate.reason)) row.append(node("p", "cc-caption", `识别说明：${str(match.reason, str(candidate.reason))}`));
      const select = node("select"); select.setAttribute("aria-label", "为这段声音标注人物"); const unknown = node("option", "", "明确标为未知 / 不确定"); unknown.value = ""; select.append(unknown);
      for (const profile of options.profiles().filter(profile => !profile.merged_into)) { const option = node("option", "", profile.display_name); option.value = profile.id; select.append(option); }
      select.value = str(segment.annotation_set ? segment.annotated_person_id : segment.automatic_person_id);
      const actions = node("div", "hh-toolbar"); const annotate = async (clear: boolean) => {
        await request("household.annotate", { speaker_segment_id: str(segment.speaker_segment_id), person_id: select.value || null,
          clear_annotation: clear, expected_revision: num(value.source_revision) });
        await openSource(str(value.conversation_turn_id)); await loadTimeline(); notice(clear ? "已撤销人工标注，自动候选仍保留供核对。" : "人物标注已保存，关联记忆将按新来源重新整理。");
      };
      actions.append(select, control("保存标注", () => annotate(false), "cc-button primary"), control("撤销人工标注", () => annotate(true)));
      if (value.source_expired === true) for (const input of Array.from(actions.querySelectorAll<HTMLButtonElement | HTMLSelectElement>("button,select"))) input.disabled = true;
      const group = num(segment.voice_group_id);
      if (group) actions.append(control("试听这组声音", () => { sourceDialog.modal.close(); options.openVoiceGroup(group); }));
      row.append(actions); body.append(row);
    }
    const edit = node("textarea"); edit.value = str(value.text); edit.rows = 4; edit.maxLength = 16000; edit.disabled = value.source_expired === true;
    const correction = node("details"); correction.append(node("summary", "", "改正转写文字"), field("原话的正确文字", edit), control("保存转写修订", async () => {
      if (!edit.value.trim()) throw new Error("请填写正确的原话。");
      await request("household.correct", { conversation_turn_id: id, text: edit.value.trim(), expected_revision: num(value.source_revision) });
      await openSource(id); await loadTimeline(); notice("转写修订已保存，旧派生记忆已失效并排队更新。");
    }, "cc-button primary")); body.append(correction);
    const revisions = node("details"); revisions.append(node("summary", "", `修订记录（${arr(value.revisions).length}）`));
    for (const raw of arr(value.revisions)) { const revision = obj(raw); revisions.append(node("p", "cc-caption", `修订 ${num(revision.revision)} · ${stamp(revision.at_ms)} · ${str(revision.reason)}`), node("p", "hh-quote", str(revision.text))); }
    body.append(revisions);
    if (arr(value.annotation_history).length) {
      const history = node("details"); history.append(node("summary", "", "人物标注记录"));
      for (const raw of arr(value.annotation_history)) { const annotation = obj(raw); history.append(node("p", "cc-caption", `${stamp(annotation.at_ms)} · ${annotation.operation === "clear" ? "撤销人工标注" : `标为 ${name(annotation.person_id)}`} · 标注修订 ${num(annotation.annotation_revision)}`)); }
      body.append(history);
    }
    if (arr(value.identity_observations).length) {
      const observations = node("details"); observations.append(node("summary", "", "声纹返回记录"));
      for (const raw of arr(value.identity_observations)) {
        const observation = obj(raw); const candidate = obj(observation.candidate);
        observations.append(node("p", "cc-caption", `${stamp(observation.at_ms)} · 对应来源修订 ${num(observation.source_revision)}${num(observation.source_revision) !== num(value.source_revision) ? " · 后续已有人工修订，自动结果不覆盖标注" : ""}`));
        for (const segment of arr(candidate.segments).length ? arr(candidate.segments) : [candidate]) {
          const match = obj(obj(segment).match ?? segment); observations.append(node("p", "cc-caption", `${label(match.status)} · ${name(match.profile_id)}${scoreText(match)}`));
        }
      }
      body.append(observations);
    }
    body.append(node("h3", "", "实际呈现回执"));
    const receipts = arr(value.receipts); if (!receipts.length) body.append(node("p", "cc-muted", "尚未收到设备呈现回执；生成回答不等于已经播出。"));
    for (const raw of receipts) { const receipt = obj(raw); body.append(node("p", "cc-caption", `${stamp(receipt.at_ms)} · ${receiptLabel(receipt)}`)); }
    const metadata = obj(value.metadata); const actions = node("div", "hh-toolbar");
    for (const artifactId of artifactIds(metadata)) actions.append(control("查看关联作品", () => openArtifact(artifactId, sourceContext(value))));
    if (str(metadata.presentation_id)) actions.append(control("查看呈现段落", () => showPresentation(str(metadata.presentation_id), body)));
    actions.append(control("忘记这一轮", async () => {
      if (!window.confirm("忘记这轮原话、人物标注及依赖它的派生记忆？")) return;
      await request("household.forget", { conversation_turn_id: id }); sourceDialog.modal.close(); await refresh(); notice("这一轮来源已忘记，相关索引清理已排队。");
    }, "cc-button danger")); body.append(actions);
  }

  let memoryGeneration = 0, taskListGeneration = 0;
  async function loadMemories(): Promise<void> {
    const version = ++memoryGeneration;
    const result = await request("household.memories", { kind: kindSelect.value || null,
      daily_conversation_id: sameDay.input.checked ? dailyId || null : null, limit: 200 });
    if (disposed || version !== memoryGeneration) return;
    memoryHost.replaceChildren();
    for (const raw of arr(result)) {
      const item = obj(raw); const card = node("article", "cc-memory-item");
      card.append(pills(label(item.kind), item.confirmed ? "已确认" : "待核对", label(item.evidence, ""),
        item.private_profile_id ? "个人范围" : "家庭共享", item.allow_cloud ? "允许云端使用" : "仅本机"), node("p", "cc-memory-text", str(item.content)));
      if (arr(item.subject_entity_ids).length) card.append(node("p", "cc-caption", `涉及人物：${arr(item.subject_entity_ids).map(name).join("、")}`));
      const learning = obj(item.learning);
      if (item.kind === "learning" && Object.keys(learning).length) {
        const evidence = node("details", "hh-learning"); evidence.append(node("summary", "", "核对题目、回答与提示"));
        if (str(learning.knowledge_point)) evidence.append(node("p", "", `知识点：${str(learning.knowledge_point)}`));
        if (str(learning.question)) evidence.append(node("p", "hh-quote", `题目：${str(learning.question)}`));
        if (str(learning.answer)) evidence.append(node("p", "hh-quote", `实际回答：${str(learning.answer)}`));
        const hints: Record<string, string> = { none: "没有提示", provided: "提供过提示", unknown: "提示情况未确定" };
        evidence.append(node("p", "cc-caption", hints[str(learning.hint_level)] ?? "提示情况未确定"));
        if (str(learning.hint_quote)) evidence.append(node("blockquote", "hh-evidence", str(learning.hint_quote)));
        const citations = node("div", "hh-toolbar");
        for (const [key, title] of [["question_source_id", "题目来源与呈现回执"], ["answer_source_id", "回答原话"], ["hint_source_id", "提示来源"], ["prior_answer_source_id", "上次作答来源"]]) {
          if (str(learning[key])) citations.append(sourceLink(learning[key], title));
        }
        if (str(learning.question_receipt_key)) evidence.append(node("p", "cc-caption", "已关联题目的设备呈现回执，可在题目来源中核对。"));
        evidence.append(citations); card.append(evidence);
      }
      for (const rawSource of arr(item.sources)) {
        const source = obj(rawSource);
        if (source.source_expired === true) card.append(node("p", "cc-caption", "原始来源已到期；此条已确认的长期记忆仍保留。"));
        else if (str(source.conversation_turn_id)) {
          if (source.quote) card.append(node("blockquote", "hh-evidence", str(source.quote)));
          card.append(sourceLink(source.conversation_turn_id, `查看来源 · 修订 ${num(source.source_revision)}`));
        }
      }
      const actions = node("div", "hh-toolbar"); actions.append(control(item.confirmed ? "取消确认" : "确认内容", async () => {
        await request("household.memory.confirm", { id: str(item.id), confirmed: !item.confirmed }); await loadMemories();
        notice(item.confirmed ? "已取消确认。" : "已确认这条记忆，原始证据仍保留。");
      }), control("忘记这条记忆", async () => {
        if (!window.confirm("忘记这条派生记忆？相关来源的再次自动提取也会被抑制，原始对话仍可单独查看。")) return;
        await request("household.memory.forget", { id: str(item.id) }); await loadMemories(); notice("派生记忆已忘记，并阻止旧来源自动重新生成它。");
      }, "cc-button danger")); card.append(actions); memoryHost.append(card);
    }
    if (!memoryHost.children.length) empty(memoryHost, "这里还没有可查看的派生记忆。新对话保留后，后台会按来源整理；模型缺失或待重试的工作可在“后台与设置”查看。");
  }
  async function loadJobs(): Promise<void> {
    const result = obj(await request("household.jobs")); if (disposed) return; jobsHost.replaceChildren();
    jobsHost.append(node("p", "cc-caption", result.graphiti_enabled === true ? "Graphiti 已启用；实际可用性请检查关系索引状态。失败的工作保留在队列中，可重试。" : "Graphiti 尚未启用。可查看本机保存的原文与记忆，关系检索暂不可用。"));
    jobsHost.append(pills(`来源清理待处理 ${num(result.source_cleanup_pending)}`, `关系索引重建待处理 ${num(result.graph_rebuild_pending)}`));
    for (const raw of arr(result.jobs)) { const job = obj(raw); const row = node("article", "hh-job");
      row.append(pills(label(job.kind), label(job.state), `已尝试 ${num(job.attempts)} 次`));
      if (job.error) row.append(node("p", "hh-error", str(job.error)));
      row.append(node("p", "cc-caption", `可处理时间：${stamp(job.available_at_ms)}`)); jobsHost.append(row);
    }
    if (!arr(result.jobs).length) jobsHost.append(node("p", "cc-muted", "当前没有待处理或失败的记忆工作。"));
  }
  async function refresh(): Promise<void> {
    if (disposed || busy) return; busy = true; const version = ++generation;
    try {
      if (panel === "tasks") { await loadTasks(); return; }
      const results = await Promise.allSettled([request("household.settings.get"), request("household.days", { limit: 90 }), loadJobs()]);
      if (disposed || version !== generation) return;
      if (results[0].status === "fulfilled") {
        settings = obj(results[0].value);
        if (!settingsDirty) { zone.value = str(settings.timezone, "UTC"); retention.value = String(Math.max(1, num(settings.retention_secs, 86400) / 86400)); history.input.checked = settings.history_enabled === true; cloud.input.checked = settings.allow_cloud === true; }
        settingsNote.textContent = `当前家庭时区 ${str(settings.timezone)} · 策略修订 ${num(settings.policy_revision)} · ${settings.raw_audio_retained === true ? "原始录音已保留" : "不保留完整录音"}`;
      }
      if (results[1].status === "fulfilled") {
        const days = arr(results[1].value).map(obj); if (!days.some(day => day.daily_conversation_id === dailyId)) dailyId = str(days[0]?.daily_conversation_id);
        daySelect.replaceChildren();
        for (const day of days) { const option = node("option", "", `${str(day.date)} · ${num(day.turn_count)} 轮 · ${str(day.timezone)}`); option.value = str(day.daily_conversation_id); daySelect.append(option); }
        daySelect.value = dailyId; await loadTimeline();
      }
      for (const result of results) if (result.status === "rejected") notice(problem(result.reason), true);
      await loadMemories();
    } finally { busy = false; }
  }
  function artifactIds(value: unknown): string[] {
    const record = obj(value); const result = new Set<string>();
    for (const candidate of [record.artifact_id, ...arr(record.artifact_ids), ...arr(obj(record.data).artifact_ids), obj(record.data).artifact_id]) if (typeof candidate === "string" && candidate) result.add(candidate);
    return [...result].slice(0, 64);
  }
  function sourceContext(source: RecordValue): RecordValue {
    const knownDay = str(source.daily_conversation_id) || (timeline.some(turn => turn.conversation_turn_id === source.conversation_turn_id) ? dailyId : "");
    return { household_id: str(source.household_id, str(settings.household_id)), daily_conversation_id: knownDay,
      conversation_turn_id: str(source.conversation_turn_id) };
  }
  async function openArtifact(id: string, context: RecordValue = {}): Promise<void> {
    const expected = artifactGeneration + 1;
    try { await loadArtifact(id, context); }
    catch (error) {
      if (!disposed && expected === artifactGeneration) artifactDialog.body.append(node("p", "hh-error", problem(error)));
      throw error;
    }
  }
  async function loadArtifact(id: string, context: RecordValue): Promise<void> {
    stopMedia(); const version = artifactGeneration; empty(artifactDialog.body, "正在读取作品…"); if (!artifactDialog.modal.open) artifactDialog.modal.showModal();
    const asset = obj(await request("artifact.get", { artifact_id: id }));
    if (disposed || version !== artifactGeneration) return;
    const body = artifactDialog.body; body.replaceChildren(node("h3", "", str(asset.title, "小呆的作品")), pills(str(asset.mime_type), sizeLabel(asset.bytes), stamp(asset.created_at_ms)));
    links(body, asset.source); const progress = node("p", "cc-caption", "正在读取本机内容…"); body.append(progress);
    const total = num(asset.bytes); const mime = str(asset.mime_type);
    if (!Number.isSafeInteger(total) || total <= 0 || total > 64 * 1024 * 1024) { progress.textContent = "这份作品超过本机预览的 64 MB 上限。请缩短媒体片段后重新生成。"; return; }
    const content = await readArtifactContent(request, asset,
      () => !disposed && version === artifactGeneration && artifactDialog.modal.open,
      percent => { progress.textContent = `正在读取本机内容 · ${percent}%`; });
    if (!content) return;
    const url = URL.createObjectURL(new Blob([content], { type: mime })); blobUrls.add(url); progress.textContent = "本机内容已读取并校验；点击播放器开始回放。";
    if (["image/jpeg", "image/png"].includes(mime)) { const image = node("img", "hh-artifact-image"); image.alt = str(asset.title, "作品图片"); image.src = url; body.append(image); }
    else if (["audio/wav", "audio/mpeg", "video/mp4"].includes(mime)) {
      const media = mime.startsWith("audio/") ? node("audio", "hh-media") : node("video", "hh-media");
      media.controls = true; media.preload = "metadata"; media.src = url;
      if (media instanceof HTMLVideoElement) media.playsInline = true;
      media.addEventListener("error", () => { progress.textContent = "当前系统无法解码这份媒体，可保存文件后检查格式。"; });
      for (const event of ["playing", "pause", "ended", "emptied", "error"]) media.addEventListener(event, playbackChanged);
      mediaElements.add(media); body.append(media);
    } else if (mime === "application/pdf") {
      const frame = node("iframe", "hh-pdf"); frame.title = "PDF 预览"; frame.sandbox.add("allow-downloads"); frame.src = url; body.append(frame);
      progress.textContent = "PDF 已校验。若系统未显示预览，可使用下方保存文件。";
    } else if (["text/plain", "text/vtt", "application/json", "image/svg+xml"].includes(mime)) {
      body.append(node("pre", "hh-text-artifact", new TextDecoder().decode(content).slice(0, 100000)));
      if (mime === "image/svg+xml") progress.textContent = "SVG 以源文本显示；可保存文件，或查看同一任务的 JPEG 画面。";
    } else progress.textContent = "当前格式提供文件保存；设备的 JPEG 帧播放器使用其对应的帧资源。";
    const extension: Record<string, string> = { "image/jpeg": "jpg", "image/png": "png", "application/pdf": "pdf", "audio/wav": "wav", "audio/mpeg": "mp3", "video/mp4": "mp4", "video/x-motion-jpeg": "mjpeg", "image/svg+xml": "svg", "application/json": "json", "text/vtt": "vtt", "text/plain": "txt" };
    const save = node("a", "cc-button", "保存文件"); save.href = url; save.download = `${str(asset.title, "xiaodai").replace(/[^\p{L}\p{N}._ -]/gu, "_").slice(0, 90)}.${extension[mime] ?? "bin"}`; body.append(save);
    if (mime === "application/pdf") {
      const print = node("div", "hh-print"); const printer = node("input"); printer.placeholder = "留空使用配置的默认打印机";
      const copies = node("input"); copies.type = "number"; copies.min = "1"; copies.max = "20"; copies.value = "1";
      const key = `console_print_${crypto.randomUUID()}`;
      print.append(field("打印机", printer), field("份数", copies));
      const printButton = control("打印这份 PDF", async () => {
        if (!str(context.household_id) || !str(context.daily_conversation_id) || !str(context.conversation_turn_id)) throw new Error("请从原始对话或任务详情打开这份作品，以保留打印的来源关联。");
        const count = Number(copies.value); if (!Number.isInteger(count) || count < 1 || count > 20) throw new Error("打印份数应为 1–20。");
        if (!window.confirm(`将“${str(asset.title, "这份 PDF")}”打印 ${count} 份？`)) return;
        const result = obj(await request("tasks.submit", { task: { household_id: str(context.household_id), daily_conversation_id: str(context.daily_conversation_id),
          conversation_turn_id: str(context.conversation_turn_id), capability: "print.submit", intent_key: key,
          input: { artifact_id: id, printer: printer.value.trim() || null, copies: count } }, confirm_effect: true }));
        notice("打印请求已建立。是否真正打印完成，以任务中的打印队列回执为准。"); artifactDialog.modal.close(); await loadTasks(); await openTask(str(result.task_id));
      }, "cc-button primary");
      if (!str(context.daily_conversation_id)) print.append(node("p", "cc-caption", "这份作品还没有完整的对话来源关联，打印前请从任务详情打开。"));
      print.append(printButton); body.append(print);
    }
  }

  const taskOverview = box("任务与作品", "资料整理、图片与媒体准备、打印有独立进度。停止机器人说话会撤销当次呈现；要结束后台工作，请在这里取消对应任务。");
  const taskStatus = node("p", "cc-caption"); const taskFilter = node("select"); taskFilter.setAttribute("aria-label", "任务状态筛选");
  for (const value of ["", "queued", "running", "awaiting_input", "succeeded", "partial", "failed", "unknown", "cancelled"]) { const option = node("option", "", value ? label(value) : "所有任务"); option.value = value; taskFilter.append(option); }
  const taskBar = node("div", "hh-toolbar"); taskBar.append(taskFilter, control("刷新任务", loadTasks)); taskOverview.append(taskBar, taskStatus);
  const taskLayout = node("div", "hh-task-layout"); const taskList = node("div", "hh-task-list"); const taskDetail = node("section", "hh-task-detail");
  taskLayout.append(taskList, taskDetail); taskOverview.append(taskLayout); tasks.append(taskOverview);
  empty(taskDetail, "选择一个任务，查看实际进度、关联来源和作品。");
  taskFilter.addEventListener("change", () => { void loadTasks().catch(error => notice(problem(error), true)); });
  async function loadTasks(): Promise<void> {
    const version = ++taskListGeneration;
    const results = await Promise.allSettled([request("tasks.list", { state: taskFilter.value || null, limit: 100 }), request("tasks.status")]);
    if (disposed || version !== taskListGeneration) return;
    if (results[0].status === "fulfilled") {
      taskRows = arr(results[0].value).map(obj); renderTasks();
      const latest = taskRows.find(task => task.task_id === currentTask?.task_id);
      if (latest && taskDetail.dataset.resumeDirty !== "true"
        && (latest.updated_at_ms !== currentTask?.updated_at_ms || latest.state !== currentTask?.state)) await openTask(str(latest.task_id));
    }
    else notice(problem(results[0].reason), true);
    if (results[1].status === "fulfilled") {
      const status = obj(results[1].value); taskStatus.textContent = status.accepting === false ? "后台任务调度尚未运行；已有记录可查看。" : "状态来自持久任务记录；外部动作的未知结果需核对，不能自动当成完成。";
    } else taskStatus.textContent = problem(results[1].reason);
  }
  function renderTasks(): void {
    taskList.replaceChildren();
    for (const task of taskRows) {
      const item = control("", () => openTask(str(task.task_id)), "hh-task-button"); item.classList.toggle("selected", task.task_id === currentTask?.task_id);
      item.append(node("strong", "", label(task.capability)), pills(label(task.state), label(task.stage, "")), node("span", "cc-caption", stamp(task.created_at_ms)));
      const result = obj(task.result); if (str(result.summary)) item.append(node("span", "hh-task-summary", str(result.summary)));
      taskList.append(item);
    }
    if (!taskRows.length) empty(taskList, "目前没有符合条件的任务。和小呆提出需要准备资料或制作图文的请求后，可在这里追踪。");
  }
  async function openTask(id: string): Promise<void> {
    if (!id) return; const version = ++taskGeneration; empty(taskDetail, "正在读取任务与回执…");
    const results = await Promise.allSettled([request("tasks.get", { task_id: id }), request("tasks.events", { task_id: id, after: 0, limit: 200 })]);
    if (disposed || version !== taskGeneration) return;
    if (results[0].status === "rejected") throw results[0].reason;
    const task = obj(results[0].value); currentTask = task; renderTasks(); taskDetail.replaceChildren(); delete taskDetail.dataset.resumeDirty;
    const result = obj(task.result); const data = obj(result.data); const context = { household_id: str(task.household_id), daily_conversation_id: str(task.daily_conversation_id), conversation_turn_id: str(task.conversation_turn_id) };
    taskDetail.append(node("h3", "", label(task.capability)), pills(label(task.state), label(task.stage), `第 ${num(task.attempt)} 次处理`),
      node("p", "hh-answer", str(result.summary, str(result.error, "等待新的进度或结果。"))), node("p", "cc-caption", `开始 ${stamp(task.created_at_ms)} · 更新 ${stamp(task.updated_at_ms)}`));
    const actions = node("div", "hh-toolbar"); actions.append(control("刷新详情", () => openTask(id)), sourceLink(task.conversation_turn_id));
    if (["queued", "running", "awaiting_input", "cancel_requested", "unknown"].includes(str(task.state))) actions.append(control("取消这个任务", async () => {
      if (task.capability === "print.submit" && !window.confirm("请求取消这次打印？已经出纸的部分无法撤回，取消结果需要打印队列确认。")) return;
      await request("tasks.cancel", { task_id: id }); await openTask(id); await loadTasks(); notice("已请求取消，最终结果请查看任务回执。");
    }, "cc-button danger"));
    if (task.capability === "print.submit" && (result.submission_started === true || data.submission_started === true)
      && !["queued", "running", "cancel_requested"].includes(str(task.state))) actions.append(control("核对执行结果", async () => {
      await request("tasks.reconcile", { task_id: id }); await openTask(id); await loadTasks(); notice("已核对已有任务；不会因此重新提交一份打印。");
    })); taskDetail.append(actions);
    const job = str(data.job_id, str(result.job_id));
    if (job) taskDetail.append(node("p", "cc-caption", `打印队列任务：${job} · ${str(data.printer, str(result.printer))}`));
    if (task.state === "unknown") taskDetail.append(node("p", "hh-error", "外部动作的结果尚不确定。请核对现有队列或回执，再决定下一步，避免重复执行。"));
    if (task.state === "awaiting_input") appendResumeForm(task, taskDetail);
    const assets = artifactIds(result); if (str(obj(task.input).artifact_id)) assets.push(str(obj(task.input).artifact_id));
    const assetBar = node("div", "hh-artifact-list");
    for (const [index, asset] of [...new Set(assets)].entries()) assetBar.append(control(`打开作品 ${index + 1}`, () => openArtifact(asset, context)));
    if (assetBar.children.length) taskDetail.append(node("h3", "", "结果作品"), assetBar);
    links(taskDetail, data); links(taskDetail, result);
    const presentationId = str(data.presentation_id, str(result.presentation_id));
    if (presentationId) taskDetail.append(control("查看呈现段落与设备回执", () => showPresentation(presentationId, taskDetail)));
    taskDetail.append(node("h3", "", "进度与执行记录"));
    if (results[1].status === "fulfilled") {
      for (const raw of arr(results[1].value)) {
        const event = obj(raw); const payload = obj(event.payload); const row = node("div", "hh-event");
        row.append(node("time", "cc-caption", stamp(event.recorded_at_ms)), node("strong", "", label(payload.stage, label(event.kind))),
          node("p", "", str(payload.summary, str(payload.message, str(payload.code)))));
        if (str(payload.job_id)) row.append(node("p", "cc-caption", `打印队列：${str(payload.job_id)}`)); taskDetail.append(row);
      }
    } else taskDetail.append(node("p", "hh-error", `进度记录暂不可用：${problem(results[1].reason)}`));
  }
  function appendResumeForm(task: RecordValue, host: HTMLElement): void {
    const id = str(task.task_id), input = obj(task.input), result = obj(task.result), data = obj(result.data);
    const form = node("form", "hh-resume cc-form"); form.append(node("h3", "", "补充信息，继续原任务"));
    form.addEventListener("submit", event => event.preventDefault());
    form.addEventListener("input", () => { taskDetail.dataset.resumeDirty = "true"; });
    if (task.capability === "print.submit") {
      const printer = node("input"); printer.value = str(input.printer, str(data.printer)); printer.maxLength = 127;
      printer.placeholder = "Mac 打印队列中的名称"; printer.required = arr(data.missing_fields).includes("printer");
      const copies = node("input"); copies.type = "number"; copies.min = "1"; copies.max = "20"; copies.required = true; copies.value = String(num(input.copies, 1));
      const paper = node("select"); paper.required = true; for (const value of ["a4", "letter", "a5", "legal"]) { const choice = node("option", "", value.toUpperCase()); choice.value = value; paper.append(choice); } paper.value = str(input.paper, "a4");
      const sides = node("select"); sides.required = true; for (const [value, title] of [["one-sided", "单面"], ["two-sided-long-edge", "双面 · 长边翻页"], ["two-sided-short-edge", "双面 · 短边翻页"]]) { const choice = node("option", "", title); choice.value = value; sides.append(choice); } sides.value = str(input.sides, "one-sided");
      const ranges = node("input"); ranges.placeholder = "留空打印全部，例如 1-3,5"; ranges.maxLength = 80; ranges.pattern = "[0-9,\\-]{1,80}"; ranges.value = str(input.page_ranges);
      let assetId = str(input.artifact_id, str(result.artifact_id, str(data.artifact_id)));
      const fileStatus = node("p", "cc-caption", assetId || input.visual ? "保留原任务的打印内容，也可重新选择一份 PDF。" : "请为这个任务选择要打印的 PDF。");
      const file = node("input"); file.type = "file"; file.accept = ".pdf,application/pdf"; file.hidden = true;
      let importing = false;
      file.addEventListener("change", () => { void (async () => {
        const chosen = file.files?.[0]; if (!chosen || importing) return;
        importing = true; fileStatus.textContent = "正在导入本机 PDF…";
        try {
          if (!chosen.size || chosen.size > 12 * 1024 * 1024) throw new Error("请选择不超过 12 MB 的 PDF。");
          const bytes = new Uint8Array(await chosen.arrayBuffer());
          if (new TextDecoder().decode(bytes.subarray(0, 5)) !== "%PDF-") throw new Error("这个文件不是可识别的 PDF。");
          let binary = ""; for (let offset = 0; offset < bytes.length; offset += 8192) binary += String.fromCharCode(...bytes.subarray(offset, offset + 8192));
          const artifact = obj(await request("artifact.import", { data_base64: btoa(binary), mime_type: "application/pdf", title: chosen.name.replace(/\.pdf$/i, "").slice(0, 200) }));
          if (disposed || !form.isConnected || currentTask?.task_id !== id) return;
          assetId = str(artifact.artifact_id); if (!assetId) throw new Error("PDF 导入没有返回可用的作品。");
          fileStatus.textContent = `已选择：${chosen.name} · ${sizeLabel(chosen.size)}`; taskDetail.dataset.resumeDirty = "true";
        } catch (error) { if (!disposed && form.isConnected) { fileStatus.textContent = problem(error); notice(problem(error), true); } }
        finally { importing = false; file.value = ""; }
      })(); });
      form.append(fileStatus, file, control("选择本机 PDF", () => file.click()), field("打印机", printer), field("份数", copies),
        field("纸张", paper), field("单双面", sides), field("页码", ranges), control("确认参数并继续打印", async () => {
          if (importing) throw new Error("PDF 仍在导入，请稍后继续。");
          if (!form.reportValidity()) return;
          if (!assetId && !input.visual) throw new Error("请先选择要打印的 PDF。");
          const count = Number(copies.value); if (!Number.isInteger(count) || count < 1 || count > 20) throw new Error("份数应为 1–20。");
          if (!window.confirm(`继续原来的打印任务：${count} 份，${printer.value.trim() || "默认打印机"}？`)) return;
          const patch: RecordValue = { printer: printer.value.trim() || null, copies: count, paper: paper.value, sides: sides.value, page_ranges: ranges.value.trim() || null };
          if (assetId) patch.artifact_id = assetId;
          await request("tasks.resume", { task_id: id, input_patch: patch, confirm_effect: true });
          await openTask(id); await loadTasks(); notice("已补充参数并继续原来的打印任务，请查看实际队列回执。");
        }, "cc-button primary"));
    } else if (task.capability === "agent.work") {
      const prompt = node("textarea"); prompt.rows = 4; prompt.maxLength = 4000; prompt.required = true; prompt.value = str(input.prompt);
      form.append(field("补充后的完整要求", prompt), control("继续原任务", async () => {
        if (!form.reportValidity() || !prompt.value.trim()) return;
        await request("tasks.resume", { task_id: id, input_patch: { prompt: prompt.value.trim() }, confirm_effect: false });
        await openTask(id); await loadTasks(); notice("已将补充要求交给原任务。");
      }, "cc-button primary"));
    } else form.append(node("p", "cc-caption", "请在对话中补充这个任务需要的信息，小呆会将补充内容关联到原任务。"));
    host.append(form);
  }
  async function showPresentation(id: string, host: HTMLElement): Promise<void> {
    const sourceVersion = sourceGeneration, taskVersion = taskGeneration;
    const results = await Promise.allSettled([request("presentation.get", { presentation_id: id }), request("presentation.receipts", { presentation_id: id })]);
    if (disposed || !host.isConnected || (host === sourceDialog.body && sourceVersion !== sourceGeneration)
      || (host === taskDetail && taskVersion !== taskGeneration)) return;
    if (results[0].status === "rejected") throw results[0].reason;
    host.querySelector(".hh-presentation")?.remove(); const view = node("section", "hh-presentation"); const plan = obj(results[0].value);
    view.append(node("h3", "", `图文与讲解 · 版本 ${num(plan.revision)}`), node("p", "cc-caption", "下方状态来自实际设备回执。Mac 预览与机器人实际播放分别记录。"));
    const receipts = results[1].status === "fulfilled" ? arr(results[1].value).map(obj) : [];
    for (const [index, raw] of arr(plan.segments).entries()) {
      const segment = obj(raw); const card = node("article", "hh-segment");
      card.append(node("h4", "", `第 ${index + 1} 段`), node("p", "hh-answer", str(segment.speech)), pills(`表情：${str(segment.expression)}`, segment.wait_for_first_frame ? "先显示画面，再讲解" : "分段展示"));
      const assets = [...arr(segment.asset_ids), segment.speech_asset_id].filter((value): value is string => typeof value === "string" && !!value);
      const actions = node("div", "hh-toolbar"); for (const [at, asset] of assets.entries()) actions.append(control(asset === segment.speech_asset_id ? "回放本段语音" : `查看画面 ${at + 1}`, () => openArtifact(asset, obj(plan.context)))); card.append(actions);
      const segmentReceipts = receipts.filter(receipt => receipt.segment_id === segment.segment_id);
      card.append(node("p", "cc-caption", segmentReceipts.length ? segmentReceipts.map(receipt => `${presentationState(receipt.state)}${str(obj(receipt.detail).code) ? ` · ${str(obj(receipt.detail).code)}` : ""}`).join(" → ") : "尚无设备回执")); view.append(card);
    }
    if (results[1].status === "rejected") view.append(node("p", "hh-error", `设备回执暂不可用：${problem(results[1].reason)}`));
    host.append(view); view.scrollIntoView({ behavior: "smooth", block: "nearest" });
  }

  const acceptance = node("details", "cc-box hh-acceptance"); acceptance.append(node("summary", "", "实机验收记录 · 开发完成后待测试"), node("p", "cc-muted", "此处仅保存人工观察，不会自动运行模型、硬件测试或 CI。新报告所有项目都从“尚未执行”开始。"));
  const environment = node("input"); environment.placeholder = "例如 Mac 型号 / 固件提交 / 房间环境"; environment.setAttribute("aria-label", "测试环境说明");
  const reportId = node("input"); reportId.placeholder = "acceptance-…"; reportId.setAttribute("aria-label", "已有验收报告 ID");
  try { reportId.value = localStorage.getItem("espbot.acceptance.report_id") ?? ""; } catch { /* ID persistence is optional. */ }
  const acceptanceBody = node("div", "hh-acceptance-items"); const acceptanceBar = node("div", "hh-toolbar");
  acceptanceBar.append(environment, control("建立空白验收记录", async () => {
    report = obj(await request("acceptance.create", { environment: { note: environment.value.trim() } })); reportId.value = str(report.report_id);
    try { localStorage.setItem("espbot.acceptance.report_id", reportId.value); } catch { /* The server already saved the record. */ }
    renderAcceptance(); notice("验收记录已建立，所有项目尚未执行。");
  }), reportId, control("打开报告", async () => { report = obj(await request("acceptance.get", { report_id: reportId.value.trim() })); renderAcceptance(); }), control("导出记录", async () => {
    if (!str(report?.report_id)) throw new Error("请先建立或打开验收记录。");
    report = obj(await request("acceptance.get", { report_id: str(report?.report_id) }));
    const url = URL.createObjectURL(new Blob([JSON.stringify(report, null, 2)], { type: "application/json" })); blobUrls.add(url);
    const download = node("a"); download.href = url; download.download = `${str(report.report_id)}.json`; download.click();
  })); acceptance.append(acceptanceBar, acceptanceBody); tasks.append(acceptance);
  function renderAcceptance(): void {
    acceptanceBody.replaceChildren(); if (!report) return;
    acceptanceBody.append(node("p", "cc-caption", `${str(report.report_id)} · 创建于 ${stamp(report.created_at_ms)}`));
    for (const raw of arr(report.scenarios)) {
      const scenario = obj(raw); const item = node("details", "hh-check-item"); item.append(node("summary", "", `${label(scenario.status)} · ${str(scenario.action)}`), node("p", "cc-caption", `应观察到：${str(scenario.expected)}`));
      const state = node("select"); state.setAttribute("aria-label", "此项实际测试状态"); for (const value of ["not_run", "passed", "failed", "blocked"]) { const option = node("option", "", label(value)); option.value = value; state.append(option); } state.value = str(scenario.status, "not_run");
      const observation = node("textarea"); observation.rows = 3; observation.maxLength = 4000; observation.value = str(scenario.observation); observation.placeholder = "填写实际观察、日志编号或失败原因";
      item.append(field("状态", state), field("实际观察", observation), control("保存此项记录", async () => {
        if (state.value === "passed" && !observation.value.trim()) throw new Error("标为通过前，请填写实际观察依据。");
        report = obj(await request("acceptance.record", { report_id: str(report?.report_id), scenario_id: str(scenario.id), status: state.value, observation: observation.value.trim() }));
        item.querySelector("summary")!.textContent = `${label(state.value)} · ${str(scenario.action)}`; notice("已保存本项人工验收记录。");
      })); acceptanceBody.append(item);
    }
  }
  const timer = window.setInterval(() => {
    if (disposed || !visible) return;
    if (panel === "tasks") void loadTasks().catch(error => notice(problem(error), true));
    else if (panel === "household" && !sourceDialog.modal.open) void loadJobs().catch(error => notice(problem(error), true));
  }, 15000);
  return {
    setPanel(next, active): void {
      panel = next; visible = active;
      if (!active || !["household", "tasks"].includes(next)) { sourceDialog.modal.close(); artifactDialog.modal.close(); stopMedia(); }
      else void refresh().catch(error => notice(problem(error), true));
    },
    refresh,
    showSource: openSource,
    showTask: openTask,
    async showMerge(): Promise<void> {
      await options.onProfilesChange?.();
      if (disposed) return;
      sections.forEach((section, index) => { section.hidden = index !== 2; });
      Array.from(tabs.children).forEach((tab, index) => { tab.classList.toggle("selected", index === 2); });
      renderMergeOptions(); merge.open = true; merge.scrollIntoView({ behavior: "smooth", block: "nearest" });
    },
    async showPresentation(id): Promise<void> {
      if (!id) return;
      sourceGeneration++;
      sourceDialog.heading.querySelector("h2")!.textContent = "图文、语音与呈现回执";
      empty(sourceDialog.body, "正在读取呈现内容…");
      if (!sourceDialog.modal.open) sourceDialog.modal.showModal();
      const version = sourceGeneration;
      try { await showPresentation(id, sourceDialog.body); }
      catch (error) { if (!disposed && version === sourceGeneration) empty(sourceDialog.body, problem(error)); throw error; }
      if (!disposed && version === sourceGeneration) sourceDialog.body.querySelector("p.cc-muted")?.remove();
    },
    stopMedia(): void {
      stopMedia();
      if (artifactDialog.modal.open) empty(artifactDialog.body, "媒体回放已停止，内容已释放。关闭此窗口后，可以再次打开作品。");
    },
    dispose(): void {
      if (disposed) return; disposed = true; generation++; sourceGeneration++; taskGeneration++; memoryGeneration++; taskListGeneration++;
      window.clearInterval(timer); stopMedia(); sourceDialog.modal.close(); artifactDialog.modal.close(); sourceDialog.modal.remove(); artifactDialog.modal.remove();
    },
  };
}
