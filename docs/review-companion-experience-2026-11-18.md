# Companion experience review: fun, responsive, family member (2026-11-18)

Scope: the whole path from wake to reply, looked at from the household's side. It covers the committed integration (`8e44f86..0385c30`) **plus Codex's uncommitted working-tree changes** (31 files, +836/−125: live early sentences, the "听到了" wait acknowledgement, the 补一句 supplement and steer, firmware supplement button/command, and `docs/review-live-turns-2026-11-18.md`). HEAD is still `0385c30`, so none of Codex's work is committed yet.

State checked on 2026-11-18 21:10 AWST (`DEVELOPER_DIR=/Library/Developer/CommandLineTools`): `xiaodai-core` `cargo test` 66 passed, 1 ignored; `bot-server/src-tauri` `cargo test` 89 passed, 2 ignored. Frontend build, firmware build and the board were not run for this review. All numbers below come from the existing docs; nothing was measured for this review.

Measured baseline (from `docs/review-live-turns-2026-11-18.md` and `docs/review-xiaodai-core-2026-11-18.md`):

| Step | Measured |
| --- | --- |
| Committed audio → "听到了" first codec write (S31, n=5) | 0.79–1.98 s, median 1.07 s |
| Committed audio → real answer first codec write (S31, n=6) | 2.89–4.14 s, median 3.81 s |
| Device end-of-speech → real answer first write (S31, n=6) | 3.10–4.35 s, median 4.02 s |
| Core first text / first sentence / core total (isolated Mac, t1) | 791 ms / 950 ms / 1355 ms |
| `brain_core` stage including 3 sentences of TTS (t1) | 4512 ms |
| Tool turn on OpenCode (weather, t3) | 17.5 s |
| Luna escalating a story it should have answered (first run) | 2.5 s wasted before the hand-off |
| Stop → Codex `interrupted` | 6.4 ms |

So the model is no longer the bottleneck (about 1 s to first text). Most of the roughly 3.8 s the family waits goes on ASR, context assembly, `say` TTS, presentation freezing and the playback handshake.

---

## P0: fix first (these break the "family member" feel)

### P0-1. Two different voices in one conversation
- **Observation:** local wake replies are Kokoro-82M `zf_001` clips (`firmware/components/bot_responses/assets-manifest.json`, 104 clips). Every Mac reply, *including* the "听到了。" acknowledgement, is macOS `say` with Tingting (`server/bot-server/local-models/worker.py:160-162`, `say_guard.py:48`, `docs/mac-bot-server.md:124`). A typical turn sounds like Kokoro saying "听见啦。", then Tingting saying "听到了。", then Tingting answering.
- **Why it matters:** a family member has one voice. Hearing two robots, with near-duplicate words a second apart, is the biggest thing stopping 小呆 feeling like a single character, and Tingting is the flattest of the three.
- **Improve:** pick one voice. Kokoro `zf_001` is already licensed (Apache-2.0), already in the repo's toolchain (`bot_responses/tools/generate_assets.py`, sherpa-onnx), and runs offline on the Mac. Add it as a `tts` backend in `worker.py` next to `say`, keep `say` as a fallback, and bench first-audio time per sentence against `say` before switching the default. `docs/upstream-reuse-catalog.md:69` already lists Kokoro, Piper and Qwen3-TTS as candidates.

### P0-2. Every core reply shows the same "happy" face
- **Observation:** `fast_core.rs:470` calls `scene::normalize(&text, None, …)` with no structured input, so `scene.rs:81-82` always picks `"happy"`, and `fast_core.rs:471-472` stamps that onto every segment. 28 expressions exist (`face_catalog_generated.rs:10`: worried, sad, laughing, affection, surprised, smug, …), but the default brain never uses them. A sad story, a scraped knee and a joke all get the same smile.
- **Why it matters:** mirroring emotion is the cheapest and most visible "it understands me" signal a companion has.
- **Improve (quick win):** have the core emit a one-token tag before each sentence, such as `〔worried〕`, validated against `EXPRESSION_IDS`, stripped by `LiveSpeech::ready` and the final segmenter, and set as the per-segment `expression`. Fall back to a tiny keyword table (哈哈/太好了 → laughing, 疼/难过 → worried/gentle, 哇 → surprised). Add a unit test next to `sentences_become_segments_within_planner_limits`.

### P0-3. The bot never knows who is talking to it during the turn
- **Observation:** `orchestration.rs:91` launches speaker ID in the background (`launch_identity`), but its result only goes to the journal (`orchestration.rs:416-417`) and never into this turn's context. `speaker_person_id` is only set for the trusted console (`orchestration.rs:87`). Recent dialogue is labelled with a generic `用户：` (`fast_core.rs:265`). The persona says "don't call anyone by name without a confirmed identity" (`fast_core.rs:23`). In practice 小呆 only uses a name when the speaker says it in the same sentence (t1, "你好，小林").
- **Why it matters:** greeting the person by name and remembering *their* preferences (not "the household's") is the core of feeling like family. With several people talking, untagged history also mixes up who said what ("你最喜欢芒果" said to the wrong child).
- **Improve:** identity already runs in parallel with ASR, so await it with a tight budget (for example 300 ms after ASR finishes) and pass `当前说话人候选：小林（声纹，未认证）` plus per-line speaker labels in `context_text`. Keep it non-authorising: it never unlocks private profiles, tools or print.
- **Needs your decision:** whether a voice match above the threshold and margin is enough to *say the name* in a greeting. The current persona and `scene.rs:113` text both forbid it. It's a privacy and product choice, so it isn't a code fix I'd make without you.

### P0-4. Failures go silent, with technical captions
- **Observation:** `brain_secure.c:420-431` maps errors to captions such as "连接或处理失败，请在 Mac 上检查" or "请在 Mac 上配置可用模型". They're text only and never spoken. A core failure after some speech has already played becomes `partial_spoken_answer` (`fast_core.rs:318-321`), so half a story is followed by an error caption.
- **Why it matters:** a child hears nothing and sees an admin message. A family member would say "哎呀，我脑袋卡住了，再说一遍好吗？".
- **Improve (quick win):** reuse the existing local "chat service offline" wake-reply pool (8 clips, `bot_responses/README.md`), or add a small "oops" pool rendered with the same Kokoro voice, and play it on `request_error` and `asr_no_speech`. Keep the technical text in the console and diagnostics, not on the face. For `partial_spoken_answer`, end with a short spoken in-character apology rather than a failure caption.

### P0-5. Codex's work is all uncommitted
- **Observation:** `git status` shows 31 modified files plus an untracked `docs/review-live-turns-2026-11-18.md`. HEAD is still `0385c30`, even though `AGENTS.md` (modified) already describes the installed-App and S31 acceptance.
- **Why it matters:** the board is running `app-flash` image `f05217…c02593` built from code that exists only in the working tree. One bad checkout loses the live-turn work.
- **Improve:** commit it in milestones (firmware supplement, server live speech/ack, core `respond_fresh`, docs) before more changes go in.

---

## P1: responsiveness and personality

### P1-1. About 2.8 s of non-model wait before the first real word
- **Observation:** the core gives first text in about 0.8–1.0 s, but the S31 median from commit to answer is 3.81 s. Contributors on the critical path, in order:
  - ASR: 1365 ms in t1, unless the early ASR candidate is reused (`orchestration.rs:94-101`).
  - Two SQLite context builds, each running `purge_household_expired()` (a write) first (`household.rs:274`, `:294`).
  - Graphiti recall, awaited serially for up to **750 ms** (`orchestration.rs:128-133`) before the core even starts.
  - TTS: the worker has one `tts` lane (`worker.rs:75`), so sentences synthesise serially through `say`. t1's `brain_core` stage was 4512 ms against a 1355 ms core.
  - Presentation freeze, `audio.playback.ready`, PCM over TCP and the first codec write.
- **Improve:**
  1. Start Graphiti recall at `turn.understood`, alongside the SQLite reads, and let the core start without it. If it arrives late, it can feed the *next* turn or a steer.
  2. Move `purge_household_expired` off the read path, onto a timer or the memory worker.
  3. Log per-stage `TurnTimings` for each device turn (ASR end, context ready, core first sentence, TTS first audio, playback.ready, first write), so the next pass is driven by data. The live-turn doc currently can't attribute the 2.8 s.
  4. P0-1's TTS swap should be benched on *first-audio-per-sentence*, not only on quality.

### P1-2. Early playback stops after the first sentence or two
- **Observation:** `LiveSpeech::ready` (`fast_core.rs:323-341`) gives up live streaming for the rest of the turn (`self.output=None`) whenever any of these happens:
  - A sentence doesn't end in `。！？!?.` (`:332`). Story dialogue ending in `。”` or `！」`, `……`, or a soft split at `max_sentence_chars=60` (`xiaodai-core/src/config.rs:355`, `splitter.rs` soft boundaries) all fail.
  - The channel is full. `publish` uses `try_reserve` (`:346`) on a capacity-2 channel (`gateway.rs:440`) that the gateway only drains when no live delivery is playing (`gateway.rs:679-680`). The unit test even asserts that the third sentence isn't sent (`fast_core.rs:585-587`).
  - The rest of the answer then waits for the whole turn plus the frozen presentation. That's exactly what makes stories feel stop-start.
- **Improve:**
  - Accept the `CLOSERS` set (`”」』）…`) as sentence ends in `ready`, so it matches `splitter.rs`.
  - Replace `try_reserve` with a bounded queue that *waits* (the gateway already serialises delivery with receipts), rather than dropping to the frozen path on the first busy moment.
  - Add a fake-device test that streams a 6-sentence story with dialogue quotes.

### P1-3. A mismatch mid-answer cancels the whole answer
- **Observation:** when the final segments don't exactly match the already-played prefix, the gateway sends `session.cancelled` with `live_presentation_changed` (`gateway.rs:732-741`). The prefix and `segment_sentences` (`fast_core.rs:482-498`) are built separately, so a split or merge difference ends the turn in silence partway through.
- **Improve:** on a mismatch, deliver only the segments after the longest common prefix (or re-segment the remaining text) instead of cancelling, and record a diagnostic.

### P1-4. The wait acknowledgement is one fixed phrase
- **Observation:** `LISTENING_ACK = "听到了。"` (`fast_core.rs:300`) plays when Luna has no first text after 550 ms (`fast_core.rs:400`). Luna's measured first text is 791–2039 ms, so it fires on most turns (with a 20 s cooldown per connection). It also comes right after a local wake reply that usually says the same thing ("听见啦。", "嗯，你说。").
- **Why it matters:** identical filler is what makes a companion sound templated.
- **Improve:** use a small pre-synthesised pool, varied by context:
  - Thinking: "嗯……让我想想。"
  - Question: "好问题！"
  - Story: "好呀，听我讲。"
  - Late evening: a softer version.
  Pick one never used in the last N turns, skip it when the wake reply was under ~5 s ago, and play it in the same voice as the answer (P0-1).

### P1-5. The persona is mostly safety rules, with little character
- **Observation:** `PERSONA` (`fast_core.rs:23`) has one clause of personality ("温和、好奇、有一点笨萌的小幽默") and about ten clauses of constraints. It doesn't say how to be funny, curious or warm. There are three diverging personas:
  - Core: whole family, 1–3 sentences.
  - OpenCode harness: "使用符合孩子年龄的简短中文" (`harness/config.rs:385`).
  - Device scene: "不预设对方是孩子" (`scene.rs:113`).
  The same family gets a different 小呆 depending on route.
- **Improve:** keep a single persona module, shared by all three, with:
  - Concrete voice guidance: one playful aside at most, light wordplay, sometimes end with one short curious question back (within the engaged session only), and react to feelings before facts.
  - 3–4 few-shot exchanges showing tone (a joke, a comfort, a "上次你说过…" callback).
  - An explicit "vary your openings; don't start with 好的/当然" rule.
  - The safety rules, condensed.
  - A version string on the core prompt, logged per turn, so a tone A/B test is possible.

### P1-6. Memory reaches the model as raw JSON and gets cut off mid-record
- **Observation:** `household_context` dumps up to 12 full JSON rows (ids, sources, evidence, subject ids…) (`household.rs:276-287`), ranked only by character-bigram overlap (`household.rs:284`, `terms` at `:589`). `context_text` then takes `memory.chars().take(limit/2)` (`fast_core.rs:258`), which can slice a JSON line in half.
- **Why it matters:** the model gets noisy metadata instead of "妈妈喜欢芒果（上周，自述）", so it rarely weaves memories naturally, and a truncated row can mislead it.
- **Improve:** render memory for the core as short natural-language bullets with relative time and who said it, trimmed on whole items. Then add the persona hint "when relevant, mention one shared memory naturally; never list them".

### P1-7. Context piles up in Luna's thread, and doesn't carry across days
- **Observation:** each turn prefixes the full reference context (up to 6000 chars) to the user text sent into the *persistent* Luna thread (`xiaodai-core/src/core.rs:475-478`). Today's dialogue is sent again as `今天的近期对话` (`fast_core.rs:276-281`, last 12 turns, `household.rs:299`), even though the thread already holds it. The thread resets on a new household day (`fast_core.rs:231-234`), and recent context covers today only.
- **Why it matters:** latency and cost grow through the day, which may be part of why t5 took 2039 ms against 791 ms in t1 (not proven). Yesterday's chat ("我明天考试好紧张") is gone tomorrow unless the extractor kept a fact.
- **Improve:**
  - Send Luna only the *delta* (new memory hits and the current speaker), since its thread already has today's history. Keep the full context for HTTP routes, which are stateless.
  - Add a short "yesterday summary" (2–3 lines per person) at the first turn of a day.

### P1-8. The tool-word pre-route sends ordinary chat to the slow path
- **Observation:** `TOOL_WORDS` (`fast_core.rs:29`) substring-matches 任务, 资料, 播放, 演示, 最新, 视频, 提醒, 链接, 后台, and more. Ordinary chat such as "今天的任务好难", "我最新的玩具", "妈妈提醒我喝水" or "我们演示了魔术" goes to OpenCode, where a tool turn took 17.5 s (t3).
- **Improve:**
  - Drop the ambiguous single words.
  - Require verb-plus-object patterns ("帮我查/搜", "设个提醒", "播放…视频").
  - Otherwise rely on the core's own `〔转交〕`.
  - Add the false-positive phrases above to `tool_tasks_private_scope_and_pending_inputs_stay_on_opencode`.

---

## P2: family-member depth and robustness

### P2-1. Memory doesn't model dates, birthdays or upcoming events
- **Observation:** extraction categories are `preference, routine, relationship, project, fact` and `event/topic/fact/learning` (`local-models/memory_extract.py:37`, `:48`). No date fields exist, so "我下周六生日" becomes free text with no anchor.
- **Improve:** add an optional `when` (absolute date, recurring yearly) to `event` and `fact` items, and a lookup "events today/this week for this speaker". Within an *already engaged* turn, the core may mention it ("对了，今天是你生日吧？"). That's compatible with the passive-by-default rule (`AGENTS.md:124`), since it only happens after an explicit wake.

### P2-2. Kids and adults get one register
- **Observation:** age is deliberately never inferred (`scene.rs:113`), and there's no declared age band anywhere in the core path.
- **Improve:** add a console-entered, guardian-set `age_band` per household person (not inferred). Pass it with the speaker candidate (P0-3) so 小呆 can use simpler words and more play for a 6-year-old, and plainer answers for adults.

### P2-3. Proactivity: what fits inside the current rule, and what needs your call
- **Fits the rule (engaged session only):**
  - Personalised first reply after a wake (name, a callback to yesterday).
  - One short follow-up question at the end of an answer.
  - A gentle "要不要我讲完下一段？" after a story.
  - A warmer sign-off when the 60 s session timeout ends (`bot_runtime.c:1461-1464`), shown as an expression or animation rather than speech.
- **Needs your decision (it would change `AGENTS.md:124`):**
  - Greeting on face presence.
  - Morning hello.
  - Birthday announcements without a wake.
  - Reminders that speak unprompted.
  Don't implement any of these without an explicit change to the passive rule.

### P2-4. Barge-in is still touch-first
- **Observation:** AEC is off, playback is half-duplex, and the spoken "补一句" fired 0 of 3 times on the board (`docs/review-live-turns-2026-11-18.md`). During waits the face shows three buttons (停止 / 重新说 / 补一句, `face.c` diff). That works, but it feels like a control panel.
- **Improve:**
  - Keep touch as the reliable path.
  - Make the most common action a tap anywhere on the face (stop while speaking, supplement while thinking), with the labelled buttons secondary.
  - Revisit spoken stop when AEC is retested.

### P2-5. Smaller code issues
- `reserve_followup` spawns a detached 10 s timer per reservation (`fast_core.rs:162-165`). It's harmless, but it can't be cancelled and isn't tied to the session.
- `turn_timeout_ms` (60 s, `fast_core.rs:56`) also covers the serial TTS drain loop (`fast_core.rs:402`). A long story with slow `say` can hit the timeout after speech has started, which turns into `partial_spoken_answer` (P0-4).
- The TTS lane waits up to 15 s and then fails with `local_worker_lane_busy` (`worker.rs:137`). The wait acknowledgement, the live sentences and the final presentation all compete for that single lane.
- `household.voice.name` and other console mutations reset Luna's thread and cancel all turns (`rpc.rs:66-70`, `mod.rs:335`). Naming a voice mid-conversation drops the conversation. Consider resetting only for forget, correction and revocation.
- The core's built-in `DEFAULT_SYSTEM_PROMPT` ("你是一个语音助手…", `xiaodai-core/src/config.rs:17`) is always overridden by bot-server. Note that in the README, so the bench numbers aren't read as persona results.

---

## Quick wins this week
1. Commit Codex's working tree in milestones (P0-5).
2. Per-sentence expression tag from the core, plus a keyword fallback (P0-2).
3. Spoken, in-character failure clips instead of technical captions (P0-4).
4. A varied wait-acknowledgement pool, skipped right after a wake reply (P1-4).
5. Accept `”」…` sentence ends and use a waiting bounded queue in `LiveSpeech` (P1-2).
6. Narrow `TOOL_WORDS`, with false-positive tests (P1-8).
7. Run Graphiti recall in parallel and take the purge off the read path (P1-1 items 1–2).
8. Render memory as whole natural-language bullets (P1-6).

## Bigger bets
1. **One voice:** Kokoro (or a benched alternative) as the Mac TTS, matching the wake clips (P0-1).
2. **Knowing who's speaking:** a bounded-wait speaker candidate in the turn context, labelled history, and a guardian-set age band. Naming policy is your decision (P0-3, P2-2).
3. **A shared persona module** with few-shot examples and version logging, used by the core, OpenCode and the scene prompts (P1-5).
4. **Delta-context Luna thread, plus yesterday summaries** for cross-day continuity (P1-7).
5. **Dated memories** (birthdays, upcoming events) surfaced only inside engaged sessions (P2-1).
6. **A per-stage latency budget** logged for every device turn, targeting under 2 s from end-of-speech to first real word (P1-1).
