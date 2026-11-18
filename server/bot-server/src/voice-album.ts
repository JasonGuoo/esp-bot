import { base64ToBuffer } from "./capture";
import "./voice-album.css";

type Group = { id: number; revision: number; name: string | null; person_id: string | null; count: number; latest_ms: number };
type Clip = { id: string; duration_ms: number; at_ms: number };
type Album = { enabled: boolean; models_configured: boolean; groups: Group[] };
type Member = { id: string; display_name: string; merged_into?: string | null };
type Request = <T>(method: string, payload?: unknown) => Promise<T>;

function node<K extends keyof HTMLElementTagNameMap>(tag: K, text = "", cls = ""): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  el.textContent = text;
  if (cls) el.className = cls;
  return el;
}

function button(label: string, action: () => void, primary = false) {
  const btn = node("button", label, primary ? "cc-button primary" : "cc-button");
  btn.type = "button";
  btn.addEventListener("click", action);
  return btn;
}

const formatter = new Intl.DateTimeFormat("zh-CN", { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
const title = (group: Group) => group.name || `未命名声音 ${group.id}`;
const ref = (group: Group) => ({ group_id: group.id, revision: group.revision });

export function mountVoiceAlbum(panel: HTMLElement, request: Request, profilesChanged: () => void, playbackChanged: (playing: boolean) => void) {
  let visible = false;
  let generation = 0;
  let active: Group | undefined;
  let members: Member[] = [];
  let album: Album;
  let url = "";
  let player: HTMLAudioElement | undefined;
  let playback = 0;
  let saving = false;
  const area = node("div", "", "va-area");
  const status = node("p", "", "va-status");
  status.setAttribute("role", "status");
  panel.replaceChildren(area, status);

  function stop() {
    playback++;
    if (player) { player.pause(); player.removeAttribute("src"); player.load(); player.remove(); player = undefined; playbackChanged(false); }
    if (url) { URL.revokeObjectURL(url); url = ""; }
  }
  function clear() { stop(); status.textContent = ""; area.replaceChildren(); }
  function error(cause: unknown) {
    const reason = String(cause);
    const message = reason.includes("voice_group_changed") ? "这组有了新片段，请刷新后再试。"
      : reason.includes("voice_choose_existing_person") ? "已有这个名字，请从家庭成员中选择。"
      : reason.includes("voice_name_required") ? "请先填写一个名字。"
      : "暂时无法完成操作，请重试。";
    status.textContent = message;
    if (reason.includes("voice_group_changed")) status.append(" ", button("刷新", () => void refresh(active?.id)));
  }

  async function change(method: string, payload: unknown, selectResult = false, success = "") {
    if (saving) return;
    const seen = generation;
    saving = true;
    try {
      const result = await request<{group_id?: number}>(method, payload);
      if (method === "household.voice.name") profilesChanged();
      if (seen !== generation || !visible) return;
      await refresh(selectResult ? result.group_id : undefined);
      if (visible && success) status.textContent = success;
    } catch (cause) { if (seen === generation) error(cause); }
    finally { saving = false; }
  }

  async function listen(group: Group, clip: Clip, row: HTMLElement, index: number) {
    stop();
    const attempt = playback;
    status.textContent = "正在读取这段录音…";
    try {
      const content = await request<{base64: string}>("household.voice.listen", { ...ref(group), segment_id: clip.id });
      if (attempt !== playback || !visible) return;
      url = URL.createObjectURL(new Blob([base64ToBuffer(content.base64, 192_044)], { type: "audio/wav" }));
      player = node("audio");
      player.controls = true;
      player.onplay = () => { if (attempt === playback) playbackChanged(true); };
      player.onpause = player.onended = () => { if (attempt === playback) playbackChanged(false); };
      player.setAttribute("aria-label", `试听片段 ${index}`);
      player.src = url;
      row.append(player);
      status.textContent = "";
      try { await player.play(); } catch { if (attempt === playback) status.textContent = "点击下方播放器的播放键即可试听。"; }
    } catch (cause) { if (attempt === playback) error(cause); }
  }

  function groupList() {
    active = undefined;
    clear();
    if (!album.models_configured) {
      area.append(node("p", "本机声音模型尚未配置好。请在「本地能力」检查声纹与说话人分段模型。", "va-callout"));
    }
    if (!album.enabled) {
      const intro = node("section", "", "cc-box va-intro");
      intro.append(node("h2", "听听家里有谁在说话"), node("p", "唤醒小呆或在控制台发送语音后，相似的声音会自动放在一组。听几段，再给这一组起名字。"),
        node("p", "只在这台 Mac 保存每段 3–6 秒的清晰原声，最多 30 天。私密对话不加入。", "cc-muted"),
        button("开启声音分组", () => void change("household.voice.settings", {enabled: true}), true));
      area.append(intro);
      return;
    }
    const head = node("div", "", "va-head");
    head.append(node("h2", `已发现 ${album.groups.length} 组声音`), button("刷新", () => void refresh()));
    area.append(head);
    if (!album.groups.length) {
      const empty = node("section", "", "cc-box va-intro");
      empty.append(node("h3", "等第一段声音"), node("p", "唤醒小呆后连续说一句三秒以上的话，或在「对话」里发送一段录音。处理完成后，第一组声音会出现在这里。"), node("p", "太短、重叠或听不清的片段会跳过。", "cc-muted"));
      area.append(empty);
    } else {
      for (const named of [false, true]) {
        const groups = album.groups.filter(g => Boolean(g.person_id) === named).sort((a, b) => b.latest_ms - a.latest_ms || a.id - b.id);
        if (!groups.length) continue;
        const section = node("section", "", "va-section");
        section.append(node("h3", `${named ? "已命名" : "待命名"} · ${groups.length}`));
        const grid = node("div", "", "va-grid");
        for (const group of groups) {
          const card = node("button", "", "va-card");
          card.type = "button";
          card.append(node("strong", title(group)), node("span", `${group.count} 段原声 · 最近于 ${formatter.format(group.latest_ms)}`), node("span", named ? "试听与修改 ›" : "试听并命名 ›", "va-link"));
          card.addEventListener("click", () => void details(group));
          grid.append(card);
        }
        section.append(grid); area.append(section);
      }
    }
    const settings = node("details", "", "va-options");
    const ask = button("关闭并清除试听片段", () => {
      settings.replaceChildren(node("summary", "声音分组设置"), node("p", "会清除所有声音组、试听片段和分组特征。家庭成员和已经确认的对话标注会保留。"),
        button("确认关闭并清除", () => void change("household.voice.settings", {enabled: false})), button("取消", groupList));
      settings.open = true;
    });
    settings.append(node("summary", "声音分组设置"), node("p", "每段只留 3–6 秒原声，最多 256 段，最长 30 天。删除对话也会删除对应片段。"), ask);
    area.append(settings);
  }

  async function details(group: Group) {
    const seen = ++generation;
    active = group; clear();
    const back = button("‹ 全部声音", () => void refresh());
    area.append(back);
    const heading = node("h2", title(group)); heading.tabIndex = -1; area.append(heading); heading.focus();
    try {
      const clips = await request<Clip[]>("household.voice.clips", ref(group));
      if (seen !== generation || !visible) return;
      const form = node("form", "", "cc-box va-form");
      const existing = node("select"); existing.setAttribute("aria-label", "选择家庭成员");
      existing.append(new Option("选择家庭成员…", ""));
      for (const member of members) existing.append(new Option(member.display_name, member.id));
      const input = node("input"); input.maxLength = 80; input.placeholder = "输入名字或家里的称呼"; input.setAttribute("aria-label", "新名字"); input.autocomplete = "off";
      existing.onchange = () => { input.disabled = !!existing.value; if (existing.value) input.value = ""; };
      input.oninput = () => { existing.disabled = !!input.value.trim(); };
      const save = node("button", group.name ? "更改归属" : "为整组命名", "cc-button primary"); save.type = "submit";
      const fields = node("div", "", "va-fields");
      if (members.length) fields.append(existing, node("span", "或"));
      fields.append(input, save);
      form.append(node("h3", group.name ? "这组声音属于谁？" : "听出来是谁了吗？"), fields);
      form.onsubmit = event => {
        event.preventDefault();
        if (!existing.value && !input.value.trim()) { status.textContent = "请选择一位成员，或输入一个新名字。"; input.focus(); return; }
        void change("household.voice.name", { ...ref(group), ...(existing.value ? {person_id: existing.value} : {name: input.value.trim()}) }, true, "已更新整组声音的名字。");
      };
      const list = node("section", "", "cc-box va-clips");
      list.append(node("h3", `试听原声 · ${clips.length} 段`));
      const more = node("details", "", "va-more");
      if (clips.length > 3) more.append(node("summary", `查看其余 ${clips.length - 3} 段`));
      clips.forEach((clip, index) => {
        const row = node("div", "", "va-clip");
        const info = node("div", "", "va-clip-info");
        info.append(node("strong", `片段 ${index + 1}`), node("span", `${formatter.format(clip.at_ms)} · ${(clip.duration_ms / 1000).toFixed(1)} 秒`));
        const play = button("▶ 试听", () => void listen(group, clip, row, index + 1)); play.setAttribute("aria-label", `试听片段 ${index + 1}`);
        row.append(info, play);
        if (clips.length > 1 || group.person_id) {
          const separate = button("这段是另一个人", () => void change("household.voice.separate", { ...ref(group), segment_id: clip.id }, true, "这段已单独分组，可以给它重新命名。"));
          separate.classList.add("quiet"); row.append(separate);
        }
        (index < 3 ? list : more).append(row);
      });
      if (clips.length > 3) list.append(more);
      area.append(list, form);
    } catch (cause) { if (seen === generation) error(cause); }
  }

  async function refresh(id?: number) {
    const seen = ++generation;
    active = undefined; stop(); status.textContent = "正在读取声音分组…";
    try {
      const [next, profiles] = await Promise.all([request<Album>("household.voices"), request<Member[]>("profile.list")]);
      if (seen !== generation || !visible) return;
      album = next; members = profiles.filter(p => !p.merged_into);
      const group = id === undefined ? undefined : album.groups.find(g => g.id === id);
      if (group) await details(group); else groupList();
    } catch (cause) {
      if (seen !== generation || !visible) return;
      area.replaceChildren(button("重新读取", () => void refresh(id))); error(cause);
    }
  }

  return {
    show(next: boolean) {
      if (next === visible) return;
      visible = next;
      if (next) void refresh(); else { generation++; clear(); active = undefined; }
    },
    open(id: number) { if (visible) void refresh(id); },
    changed() { if (visible && !active && !saving) void refresh(); },
    stop,
  };
}
