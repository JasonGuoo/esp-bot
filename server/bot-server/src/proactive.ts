// 主动打招呼 (UX review §12.2): the owner switch, per-moment toggles, quiet
// hours, guardian reminders and today's journal of unprompted lines. The Mac
// only proposes a line; the robot decides the moment and only ever speaks.

type Request = <T>(method: string, payload?: unknown) => Promise<T>;
type Settings = { enabled: boolean; morning_hello: boolean; dated_events: boolean; reminders: boolean; missed_you: boolean;
  quiet_start: string; quiet_end: string; gap_minutes: number; daily_cap: number };
type Reminder = { id: string; text: string; due_at_ms: number; state: string; attempts: number };
type Line = { offer_id: number; at_ms: number; moment: string; trigger_source: string; line: string; status: string; reason: string | null };

const MOMENTS: [keyof Settings, string, string][] = [
  ["morning_hello", "早上问好", "安静时段结束后到上午 11 点，第一次看到有人时问声早。"],
  ["dated_events", "今天的日子", "家庭记忆里记下日期的事（生日、活动），当天提一句。"],
  ["reminders", "家长设的提醒", "到点时有人在就说出来；没人就显示在屏幕上，30 分钟后再试一次。"],
  ["missed_you", "好久不见", "两天以上没聊过，再见到人时说一句想你了。"],
];
const MOMENT_NAMES: Record<string, string> = { morning: "早上问好", event: "今天的日子", reminder: "提醒", missed_you: "好久不见" };
const STATUS_NAMES: Record<string, string> = { offered: "等待合适时机", started: "已说出", declined: "未说（Mac 拒绝）", expired: "没遇到合适时机",
  shown: "显示在屏幕上", lost: "设备未回报" };
const REASONS: Record<string, string> = { disabled: "开关已关", moment_off: "该类已关", busy: "正在忙", quiet_hours: "安静时段",
  daily_cap: "已到每日上限", gap: "间隔太短", repeated: "今天说过了", reminder_gone: "提醒已取消" };
const REMINDER_STATES: Record<string, string> = { pending: "待提醒", spoken: "已说出", shown: "已显示", missed: "没人听到", cancelled: "已取消" };

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = ""): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag); if (cls) node.className = cls; if (text) node.textContent = text; return node;
}
const time = new Intl.DateTimeFormat("zh-CN", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

export function mountProactive(panel: HTMLElement, request: Request, notice: (text: string, error?: boolean) => void) {
  let visible = false, generation = 0;
  let settings: Settings | null = null;
  const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);

  async function save(patch: Partial<Settings>) {
    try { settings = await request<Settings>("proactive.settings.set", patch); notice("主动打招呼设置已保存"); }
    catch (error) { notice(errorText(error), true); }
    void refresh();
  }

  function check(label: string, hint: string, checked: boolean, disabled: boolean, change: (value: boolean) => void) {
    const row = el("label", "cc-check"); const box = el("input"); box.type = "checkbox"; box.checked = checked; box.disabled = disabled;
    box.addEventListener("change", () => change(box.checked));
    row.append(box, el("span", "", label)); if (hint) row.title = hint;
    return row;
  }

  function render(current: Settings, reminders: Reminder[], lines: Line[]) {
    const main = el("div", "cc-box");
    main.append(el("div", "cc-section-title", "主动打招呼"),
      el("p", "cc-muted", "打开后，小呆会在白天、安静、看到有人、没有在聊天或忙别的时候，偶尔主动说一句。只说话，不会因此开始听；要聊天仍然需要叫醒它。默认关闭。"),
      check("主动打招呼", "", current.enabled, false, value => void save({ enabled: value })));
    const moments = el("div", "cc-form");
    for (const [key, label, hint] of MOMENTS)
      moments.append(check(label, hint, Boolean(current[key]), !current.enabled, value => void save({ [key]: value } as Partial<Settings>)));
    main.append(moments);
    const limits = el("div", "cc-two-fields");
    const field = (label: string, input: HTMLInputElement) => { const wrap = el("label", "cc-caption", label); wrap.append(input); return wrap; };
    const start = el("input"); start.type = "time"; start.value = current.quiet_start;
    const end = el("input"); end.type = "time"; end.value = current.quiet_end;
    const gap = el("input"); gap.type = "number"; gap.min = "30"; gap.max = "1440"; gap.value = String(current.gap_minutes);
    const cap = el("input"); cap.type = "number"; cap.min = "1"; cap.max = "10"; cap.value = String(current.daily_cap);
    limits.append(field("安静时段开始", start), field("安静时段结束", end), field("两次之间至少（分钟）", gap), field("全家每天最多（次）", cap));
    const apply = el("button", "cc-button", "保存时段与次数"); apply.type = "button";
    apply.addEventListener("click", () => void save({ quiet_start: start.value, quiet_end: end.value, gap_minutes: Number(gap.value), daily_cap: Number(cap.value) }));
    main.append(limits, apply, el("p", "cc-muted", "音量始终跟随机器人上设置的音量；夜间、设置页打开、正在播放或周围有人说话时都不会开口。还认不出是谁，所以问候不带名字。"));

    const reminderBox = el("div", "cc-box");
    reminderBox.append(el("div", "cc-section-title", "提醒"));
    const add = el("div", "cc-inline");
    const text = el("input"); text.placeholder = "例如：该喝水啦"; text.maxLength = 80;
    const due = el("input"); due.type = "datetime-local";
    const addButton = el("button", "cc-button", "添加提醒"); addButton.type = "button";
    addButton.addEventListener("click", async () => {
      const at = due.value ? new Date(due.value).getTime() : NaN;
      if (!text.value.trim() || !Number.isFinite(at)) { notice("请填写提醒内容和时间", true); return; }
      try { await request("proactive.reminder.add", { text: text.value.trim(), due_at_ms: at }); notice("提醒已添加"); }
      catch (error) { notice(errorText(error), true); }
      void refresh();
    });
    add.append(text, due, addButton);
    const list = el("div", "cc-row-list");
    for (const reminder of reminders) {
      const row = el("div", "cc-inline");
      row.append(el("span", "", `${time.format(reminder.due_at_ms)}  ${reminder.text}`), el("span", "cc-muted", REMINDER_STATES[reminder.state] ?? reminder.state));
      if (reminder.state === "pending") {
        const cancel = el("button", "cc-button quiet", "取消"); cancel.type = "button";
        cancel.addEventListener("click", async () => {
          try { await request("proactive.reminder.cancel", { id: reminder.id }); } catch (error) { notice(errorText(error), true); }
          void refresh();
        });
        row.append(cancel);
      }
      list.append(row);
    }
    if (!reminders.length) list.append(el("p", "cc-muted", "还没有提醒。"));
    reminderBox.append(add, el("p", "cc-muted", current.enabled && current.reminders ? "提醒也受安静时段和每日上限限制。" : "要让提醒说出来，请先打开“主动打招呼”和“家长设的提醒”。"), list);

    const journal = el("div", "cc-box");
    journal.append(el("div", "cc-section-title", "今天主动说过的话"));
    const entries = el("div", "cc-row-list");
    for (const line of lines) {
      const row = el("div", "cc-inline");
      const reason = line.reason ? `（${REASONS[line.reason] ?? line.reason}）` : "";
      row.append(el("span", "cc-muted", `${time.format(line.at_ms)} · ${MOMENT_NAMES[line.moment] ?? line.moment}`),
        el("span", "", line.line), el("span", "cc-muted", `${STATUS_NAMES[line.status] ?? line.status}${reason}`));
      entries.append(row);
    }
    if (!lines.length) entries.append(el("p", "cc-muted", "今天还没有。"));
    journal.append(entries);
    panel.replaceChildren(main, reminderBox, journal);
  }

  async function refresh() {
    if (!visible) return;
    const current = ++generation;
    try {
      const today = new Date(); const pad = (n: number) => String(n).padStart(2, "0");
      const date = `${today.getFullYear()}-${pad(today.getMonth() + 1)}-${pad(today.getDate())}`;
      const [loaded, reminders, journal] = await Promise.all([
        request<Settings>("proactive.settings.get"),
        request<{ reminders: Reminder[] }>("proactive.reminders", { limit: 50 }),
        request<{ lines: Line[] }>("proactive.journal", { local_date: date, limit: 50 }),
      ]);
      if (current !== generation || !visible) return;
      settings = loaded;
      render(loaded, reminders.reminders, journal.lines);
    } catch (error) {
      if (current === generation) panel.replaceChildren(el("p", "cc-muted", `暂时无法读取主动打招呼设置：${errorText(error)}`));
    }
  }

  return {
    show(next: boolean) { if (next === visible) return; visible = next; if (next) void refresh(); else generation++; },
    changed() { if (visible) void refresh(); },
    get settings() { return settings; },
  };
}
