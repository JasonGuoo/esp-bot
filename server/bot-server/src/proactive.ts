type Request = <T>(method: string, payload?: unknown) => Promise<T>;
type Reminder = { id: string; text: string; due_at_ms: number; state: string };
type Line = { at_ms: number; line: string; status: string };

const STATES: Record<string, string> = {
  pending: "待处理", spoken: "已说出", shown: "已显示", missed: "未送达", cancelled: "已取消",
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
      const [reminders, journal] = await Promise.all([
        request<{ reminders: Reminder[] }>("proactive.reminders", { limit: 50 }),
        request<{ lines: Line[] }>("proactive.journal", { limit: 50 }),
      ]);
      if (current !== generation || !visible) return;

      const reminderBox = el("div", "cc-box");
      reminderBox.append(el("div", "cc-section-title", "已保存的提醒"),
        el("p", "cc-muted", "小呆在唤醒或触摸后才开始对话。已保存的提醒暂不会自动播放。"));
      const list = el("div", "cc-row-list");
      for (const reminder of reminders.reminders) {
        const row = el("div", "cc-inline");
        row.append(el("span", "", `${time.format(reminder.due_at_ms)}  ${reminder.text}`),
          el("span", "cc-muted", STATES[reminder.state] ?? reminder.state));
        if (reminder.state === "pending") {
          const cancel = el("button", "cc-button quiet", "取消");
          cancel.type = "button";
          cancel.addEventListener("click", async () => {
            try { await request("proactive.reminder.cancel", { id: reminder.id }); }
            catch (error) { notice(error instanceof Error ? error.message : String(error), true); }
            void refresh();
          });
          row.append(cancel);
        }
        list.append(row);
      }
      if (!reminders.reminders.length) list.append(el("p", "cc-muted", "没有已保存的提醒。"));
      reminderBox.append(list);

      const history = el("div", "cc-box");
      history.append(el("div", "cc-section-title", "此前的主动播报记录"));
      const entries = el("div", "cc-row-list");
      for (const line of journal.lines) {
        entries.append(el("p", "cc-muted", `${time.format(line.at_ms)} · ${line.line} · ${line.status}`));
      }
      if (!journal.lines.length) entries.append(el("p", "cc-muted", "没有历史记录。"));
      history.append(entries);
      panel.replaceChildren(reminderBox, history);
    } catch (error) {
      if (current === generation) panel.replaceChildren(el("p", "cc-muted",
        `暂时无法读取提醒记录：${error instanceof Error ? error.message : String(error)}`));
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
