# UX plan implementation summary, 2026-11-18

**2026-11-19 clarification:** the unprompted-greeting feature in phase 5 was subsequently
disabled by owner direction. This document records the 2026-11-18 snapshot. Current behavior is
described in [the behavior lifecycle](behavior-lifecycle.md).


This summarises how [the merged UX review](review-ux-2026-11-18.md) was implemented. All work is
on `main` with `[skip ci]` and has not been pushed. Each commit builds on its own.

**Evidence levels used below:**

- **Host:** unit, integration or fake-device tests on the Mac.
- **Build:** the IDF firmware build, and `npm run build` for the console.
- **Board boot:** app-only flash to the S31, then a 45 s serial capture. It shows the app version,
  the preserved owner volume (17), "Authenticated Mac gateway connected" and no panic. Boot proves
  the firmware starts. It does not prove a behaviour.

Nothing in the plan has been accepted end to end on the S31 with the new App. The user's App was
not restarted, because restarting would drop the model keys held in its session. The running App
is therefore older than this code.

## Phases

The phase grouping is reconstructed from the commit times and the §9 rows.

| Phase | Delivered | Commits | Evidence |
| --- | --- | --- | --- |
| 1 (P0) | Live-turn work committed in milestones, including the 补一句 button. Streaming fixes: quotes, a full queue, regrouped plans. A validated per-sentence emotion tag shown while speaking. | `d4e8264` `b84869d` `d7e30a3` `a26653d` `07d5e10` `e6cd27c` `199f56d` `4a92e38` `771da6b` | Host: server 89 passed / 2 ignored, core 26 + 28 passed, npm build. IDF build only. |
| 2 (P0/P1) | In-character failure captions and spoken failure lines. Kokoro `zf_001` TTS backend with a `say` fallback and a priority lane (Kokoro not installed into production folders). Graphiti recall beside SQLite with a 300 ms cap. Per-stage latency budgets. Narrow tool routing with false-positive tests. One shared persona. A varied pre-synthesised ack pool. Memory as dated notes, speaker-labelled history, and names from a confident voice match with corrections. Console controls for these. | `9f1fe6f` `835aecd` `364455d` `c6b0bc3` `d457d3a` `639d04b` `31ecd1b` `2963a0d` `4de6b32` `da59f47` | Host tests. Board boot at `364455d`. |
| 3 (P1 face) | Nod on submit, scanning while thinking with an "aha" pop, eyes following the playback envelope, two-line captions. | `c461baf` | Host tests (symmetric idle eyes). Board boot at `da59f47-dirty`. |
| 4 (P2) | Luna delta context and a previous-day summary. Dated memories and a guardian age band (additive tables). Goodbye intents and a story-continuation offer. Contextual tap, affection, poke storms, drag gaze, flinch, yawn, variable blinks, rare wink/smug. Console Friend view with a live face, device mirror, today strip, streaming text, hubs and `?` help. | `c9012b3` `2a77e02` `6e7db7c` `4fc483a` `82181ac` | Host tests. Delta bench: input 862–864 chars full vs 72–74 delta. TTFT medians drifted with the service and showed no stable gain. Headless console screenshot. Board boot at `6e7db7c-dirty`. App free space 0xa0d0 → 0x9520. |
| 5 (P2 §12.2) | Proactive greeting, event and reminder engine (details below). | `48c62a1` `a8015f3`, plus the docs commit | Host and fake-device tests. Board boot at `a8015f3`: volume 17, gateway connected at about 32 s, no panic. The running App predates `proactive.v1`, so no offer can reach the board yet. App free space 0x9520 → 0x85d0. |

### Phase 5 in short

- **Mac (proposes):** settings, reminders and a journal live in additive tables, so the database
  schema version is unchanged.
  - A pure planner covers morning hello, today's dated events, guardian reminders and missed-you
    (2+ days).
  - The owner switch is off by default. Each moment toggle defaults on. Quiet hours default to
    21:30–07:30, the gap to 180 min and the household cap to 5. The same line is never repeated on
    a day.
  - The gateway sends `proactive.offer` only to firmware that sent `proactive.ready`. It re-checks
    every guardrail on `proactive.accept`, including foreground turns and queued or running tasks.
  - It rejects any input on a proactive session.
  - The console has a 家人 → 主动打招呼 page.
- **Device (decides and speaks):** `bot_runtime` keeps the offer and picks the moment.
  - The moment needs a face for ≥1.5 s, no nearby speech for 5 s, no interaction for 60 s and
    ≥20 min since the last line. It must be daytime and not dark, with settings closed, no
    session or playback, spoken answers on and not muted.
  - The line plays on a speaker-only lease: never `audio_session_requested`, capture, VAD or
    uplink, and no follow-up window. It ends after the line, plays at the owner volume and is
    always nameless.
  - A wake or tap replaces the line with a normal session.
  - An offer nobody hears expires. A reminder is shown on screen instead and retried once.
- **Tests:**
  - firmware host test `test_proactive.c` (8 scenarios, ASan/UBSan);
  - Rust planner and storage tests (8 tests);
  - a fake device in the gateway TLS test: offer after ready, unknown or silent opens rejected,
    `audio.begin`/`assistant.text` rejected, accept refused after the owner switch is turned off,
    and the refusal journalled.

## Final test numbers (phase 5 tree)

- `cargo test` (bot-server): 113 passed, 0 failed, 3 ignored.
- Firmware host: `HOST_RUNTIME_TEST_PASS` and `HOST_PROACTIVE_TEST_PASS`.
- `npm run build`: passes.
- IDF build: passes, 0x85d0 bytes free in the app partition.
- **Not run in phase 5:** the xiaodai-core crate tests (the crate was not touched).

## Host-only versus board-verified

- **Board-verified:** each flashed build boots with the owner volume kept at 17, connects to the
  Mac gateway, and does not panic. The boots were at `364455d`, `da59f47-dirty`, `6e7db7c-dirty`
  and `a8015f3`.
- **Host-only:** every behaviour in the table.
- **Notes on specific items:**
  - The ambient-sound `speech` flag that blocks proactive lines is the existing bot_audio
    estimate: AFE VAD plus a recent loud peak. It is emitted at most every 1.2 s during sound
    activity. How reliable it is while people talk has not been measured on the board.
  - The mic-peak flinch threshold (20000) is untuned.
  - Kokoro is not installed, so `say` remains the voice until it is.

## Rollback notes

- No `brain.json` fields were added in phases 4–5, because settings use `deny_unknown_fields`.
  New settings live in additive tables read with serde defaults, so an older App still opens the
  database.
- Firmware changes are app-only. The partition table, provisioning and model/data partitions are
  untouched.
- Old firmware never receives offers, because the Mac waits for `proactive.ready`. New firmware
  never sends proactive messages to a Mac that lacks `proactive.v1`.

## Not built

- The storage-partition wake-pool loader. It needs the user's approval.
- Earcon playback.
- The "answer directly" follow-up window (default off).
- Pre-wake identity, so unprompted lines stay nameless.
- AEC voice barge-in (P3).

## Checklist for the user once the new App is installed

The new App must be started from this tree, with keys re-entered in its session.

1. **Console 家人 → 主动打招呼:** it shows the switch off and the defaults above. Turn it on, and
   check that a save shows "主动打招呼设置已保存".
2. **Reminder:** add a reminder 2 minutes ahead. Sit in front of the S31 in daylight, quietly.
   - It should say "提醒一下：…" at owner volume with a happy face, then return to idle.
   - The status line must not show "我在听".
   - The journal shows "已说出".
3. **Reminder, absent:** repeat with nobody in front of the S31. After about 10 min the text appears
   on screen, the reminder is retried once after 30 min, and the journal shows "显示在屏幕上".
4. **Talking blocks the line:** talk near the S31 while a line is pending. Nothing is said until 5 s
   of quiet.
5. **Tap during the line:** tap while it prepares. A normal listening session starts. A tap while it
   speaks stops it.
6. **Quiet hours:** set them to cover now. Nothing is offered; the journal stays empty or shows
   "安静时段".
7. **Morning hello:** next morning after 07:30, the first face gets one nameless hello, plus today's
   dated event if there is one. There is no second hello that day.
8. **Serial log:** look for "Proactive offer … received". Check that no `audio.begin` follows a
   proactive `session.open`.
9. **Earlier phases, end to end:**
   - emotion faces while speaking;
   - the spoken failure line;
   - the varied ack;
   - a name after a confident voice match, and "我不是…" correction;
   - nod, scan and "aha" eyes;
   - two-line captions;
   - goodbye ending the session;
   - the story-continuation offer;
   - tap, long-press, rub, poke storm, drag, bang flinch;
   - the console Friend view with the live face and streaming text.
10. **Volume:** confirm it is still the owner setting after all of the above.
