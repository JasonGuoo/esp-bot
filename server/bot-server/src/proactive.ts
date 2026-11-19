type Request = <T>(method: string, payload?: unknown) => Promise<T>;
type Settings = { enabled: boolean; reminders: boolean; quiet_start: string; quiet_end: string; daily_cap: number };
type Reminder = { id: string; text: string; due_at_ms: number; state: string };
type Line = { at_ms: number; line: string; status: string };

const LINE_STATES: Record<string, string> = {
  offered: "等待播放时机", preparing: "准备中", playing: "正在播放", spoken: "已播放",
  declined: "未播放", expired: "已过期", shown: "已显示", lost: "播放未确认",
};
const STATES: Record<string, string> = {
  pending: "待提醒", spoken: "已说出", shown: "已显示", missed: "未送达", cancelled: "已取消",
};
const time = new Intl.DateTimeFormat("zh-CN", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls = "", text = ""): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (cls) node.className = cls;
  if (text) node.textContent = text;
  return node;
}

export function mountProactive(panel: HTMLElement, request: Request, notice: (text: string, error?: boolean) => void) {
  let visible = false, generation = 0;

  async function refresh() {
    if (!visible) return;
    const current = ++generation;
    try {
      const [settings, reminders, journal] = await Promise.all([
        request<Settings>("proactive.settings.get"),
        request<{ reminders: Reminder[] }>("proactive.reminders", { limit: 50 }),
        request<{ lines: Line[] }>("proactive.journal", { limit: 50 }),
      ]);
      if (current !== generation || !visible) return;

      const options = el("div", "cc-box");
      options.append(el("div", "cc-section-title", "定时提醒"),
        el("p", "cc-muted", "小呆只主动播报家人明确设置的提醒，到点后无需唤醒或露脸。安静时段、夜间模式、静音、设备忙碌或附近说话会推迟播放。"));
      const enabled = el("input");
      enabled.type = "checkbox";
      enabled.checked = settings.enabled && settings.reminders;
      const enabledLabel = el("label", "cc-check");
      enabledLabel.append(enabled, el("span", "", "允许定时提醒播报"));
      options.append(enabledLabel);

      const fields = el("div", "cc-two-fields");
      const start = el("input"); start.type = "time"; start.value = settings.quiet_start;
      const end = el("input"); end.type = "time"; end.value = settings.quiet_end;
      const cap = el("input"); cap.type = "number"; cap.min = "1"; cap.max = "10"; cap.value = String(settings.daily_cap);
      for (const [label, input] of [["安静时段开始", start], ["安静时段结束", end], ["每天最多播放次数", cap]] as const) {
        const wrap = el("label", "cc-caption", label);
        wrap.append(input); fields.append(wrap);
      }
      const save = el("button", "cc-button", "保存提醒设置");
      save.type = "button";
      save.addEventListener("click", async () => {
        try {
          await request("proactive.settings.set", {
            enabled: enabled.checked, reminders: enabled.checked,
            quiet_start: start.value, quiet_end: end.value, daily_cap: Number(cap.value),
          });
          notice("提醒设置已保存");
        } catch (error) { notice(error instanceof Error ? error.message : String(error), true); }
        void refresh();
      });
      options.append(fields, save, el("p", "cc-caption", "设备两次提醒至少间隔 20 分钟；未播出会稍后重试一次，超过 3 小时仍未送达则标记为未送达。"));

      const reminderBox = el("div", "cc-box");
      reminderBox.append(el("div", "cc-section-title", "已设置的提醒"));
      const add = el("div", "cc-inline");
      const text = el("input"); text.placeholder = "例如：该喝水啦"; text.maxLength = 80;
      const due = el("input"); due.type = "datetime-local"; due.setAttribute("aria-label", "提醒时间");
      const addButton = el("button", "cc-button", "添加提醒"); addButton.type = "button";
      addButton.addEventListener("click", async () => {
        const at = due.value ? new Date(due.value).getTime() : NaN;
        if (!text.value.trim() || !Number.isFinite(at)) { notice("请填写提醒内容和时间", true); return; }
        try { await request("proactive.reminder.add", { text: text.value.trim(), due_at_ms: at }); notice("提醒已添加"); }
        catch (error) { notice(error instanceof Error ? error.message : String(error), true); }
        void refresh();
      });
      add.append(text, due, addButton); reminderBox.append(add);
      const list = el("div", "cc-row-list");
      for (const reminder of reminders.reminders) {
        const row = el("div", "cc-inline");
        row.append(el("span", "", `${time.format(reminder.due_at_ms)}  ${reminder.text}`),
          el("span", "cc-muted", STATES[reminder.state] ?? reminder.state));
        if (reminder.state === "pending") {
          const cancel = el("button", "cc-button quiet", "取消"); cancel.type = "button";
          cancel.addEventListener("click", async () => {
            try { await request("proactive.reminder.cancel", { id: reminder.id }); }
            catch (error) { notice(error instanceof Error ? error.message : String(error), true); }
            void refresh();
          });
          row.append(cancel);
        }
        list.append(row);
      }
      if (!reminders.reminders.length) list.append(el("p", "cc-muted", "还没有提醒。"));
      reminderBox.append(list);

      const history = el("div", "cc-box");
      history.append(el("div", "cc-section-title", "播报记录"));
      const entries = el("div", "cc-row-list");
      for (const line of journal.lines) entries.append(el("p", "cc-muted", `${time.format(line.at_ms)} · ${line.line} · ${LINE_STATES[line.status] ?? line.status}`));
      if (!journal.lines.length) entries.append(el("p", "cc-muted", "没有历史记录。"));
      history.append(entries);
      panel.replaceChildren(options, reminderBox, history);
    } catch (error) {
      if (current === generation) panel.replaceChildren(el("p", "cc-muted",
        `暂时无法读取提醒设置：${error instanceof Error ? error.message : String(error)}`));
    }
  }

  return {
    show(next: boolean) {
      if (next === visible) return;
      visible = next;
      if (next) void refresh(); else generation++;
    },
    changed() { if (visible) void refresh(); },
  };
}
