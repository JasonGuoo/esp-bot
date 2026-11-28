# xiaodai-fast-responder: dual-engine core

> **In esp-bot (2026-11-18):** this crate was copied here from `~/grok-bot/xiaodai-fast-responder` as source
> (no `runtime/`, `target/` or `.env`). The Mac bot-server (`server/bot-server/src-tauri`) uses it as a path
> dependency and makes it the default conversational brain; see `docs/review-xiaodai-core-2026-11-18.md`.
> Run `cargo test` here; with the Xcode licence unaccepted, prefix `DEVELOPER_DIR=/Library/Developer/CommandLineTools`.

A small, standalone Rust core: **text in → fast streamed sentences out**, interruptible, with two engines:

1. **Codex engine** (primary): one long-lived `codex app-server --stdio` child using your existing Codex
   login, model **`gpt-6-luna`** at the lowest effort it supports (**`low`**).
2. **HTTP engine** (fallback): GLM / DeepSeek Flash (/ Qwen) over OpenAI-compatible streaming, racing.

No memory, ASR, TTS or device protocol here; a brain wraps it.

## API

```rust
use fast_responder::{Core, Config, EngineMode, Event, CancellationToken};

let core = Core::new(Config::default())?;   // mode = Dual, workspace_root = "/Users/jason/Xiaodai_Server"
core.warmup().await;                        // HTTP pre-connect + Codex spawn/initialize/model list/primed thread
let cancel = CancellationToken::new();
let mut turn = core.respond("你好", cancel.clone());
while let Some(ev) = turn.next().await {
    match ev {
        Event::FirstToken { route, ms } => {}       // route = "codex/gpt-6-luna" or "deepseek-flash" ...
        Event::Sentence { index, text, ms } => {}   // hand to TTS as it arrives
        Event::Done { text, timings } => break,
        Event::Cancelled { text, timings } => break,
        Event::Error { message, timings } => break,
    }
}
// cancel.cancel() / turn.cancel() / dropping `turn` stops it at once (Codex: turn/interrupt is sent).
```

`Timings`: `engine` ("codex"/"http"), `route`, `routes_started`, `first_token_ms`, `first_sentence_ms`,
`total_ms`, `reasoning_chars`, `errors`, `superseded`, plus (new) `luna` ("preferred" / "degraded: <why>"),
`network` (winning HTTP route's path: "en0" or "system"), `attempts` (every attempt started: route, start ms,
its own TTFT, outcome ok/lost/failed/timeout, network, error) and `latency` (rolling per-engine stats after
this turn). `core.health()` returns the same rolling stats + Luna state + recent switches; `core.luna_state()`,
`core.active_interface()`. Extras: `core.warmup_codex()` (cold-start report),
`core.codex().last_interrupt()` / `wait_interrupt_completed()`, `last_steer()` (ack ms, steer→first token ms),
`thread_id()`, `threads_started()`, `turn_active()` / `wait_idle()`, `audit()` (approval decisions).

## Modes (`Config.mode`)

- `Dual` (default): Codex starts at t=0. If it has shown no visible text after `codex.fallback_after_ms`
  (**2000**, chosen from the bench: warm Codex usually answers in 1.4-2.5 s), the HTTP race starts too.
  First visible text wins and the loser is cancelled (a losing Codex turn gets `turn/interrupt`).
  A Codex error, crash, missing login or missing model starts HTTP **immediately**.
- `CodexOnly`, `HttpOnly`.

## Workspace access (`Config.workspace_root`)

Default `/Users/jason/Xiaodai_Server` (override with `XIAODAI_WORKSPACE_ROOT`).
- Codex: thread `cwd` = root, sandbox `workspace-write`, each turn sends
  `sandboxPolicy {type: workspaceWrite, writableRoots: [root], networkAccess: true}`, `approvalPolicy: never`.
  If an approval request still arrives, it is **accepted only when everything it touches is inside the root**
  (command cwd + path-like arguments; file-change paths; permission paths), otherwise declined.
  `currentTime/read` is answered; unknown server requests get a JSON-RPC error, so a turn never stalls.
- HTTP: `workspace_root` is in `Config` for the brain to use (no tools yet).

## Codex engine details (`src/codex.rs`, own thin client)

- Binary: `codex.binary`, else `XIAODAI_CODEX_BIN`, else **`/usr/local/bin/codex`** (0.170 since the
  2026-11-18 upgrade), else the ChatGPT app's bundled codex
  (`/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex`, 0.169), else
  `~/working/codex-cli/node_modules/.bin/codex`, else `codex` on PATH.
  gpt-6-luna needs codex **0.169 or newer**: with 0.153.4 every turn failed with HTTP 400 from the ChatGPT
  backend ("The 'gpt-6-luna' model is not supported when using Codex with a ChatGPT account"), even when a
  catalog listed the model; `--enable realtime_conversation` and `includeHidden` changed nothing.
- **Static model catalog** (`codex.static_model_catalog: true`): at spawn the core runs
  `<codex> debug models --bundled`, writes it to `$TMPDIR/xiaodai-core/codex-models-*.json` and passes
  `-c model_catalog_json="<file>"` to this child only. Without it the app-server re-fetches the remote model
  list, which on this network times out after 5 s and was retried on `model/list`, `thread/start` and **every
  turn** (warm TTFT ~5.7 s instead of ~1.6 s). `codex.model_catalog_json` points at your own file instead.
- Handshake `initialize` (clientInfo `xiaodai_core`, `experimentalApi: true`) → `initialized` → `account/read`
  (not logged in = unhealthy) → `model/list` (exact match on the configured model, never substituted; missing
  model = error listing what exists) → `thread/start` (ephemeral) → one throwaway **priming turn**, because the
  first turn on any new thread costs ~10-13 s of session setup.
- **One thread for the whole conversation**: every turn reuses the same `threadId`, including after interrupts
  and steers (no spare threads). `codex.max_turns_per_thread` (default 0 = never) can force rotation.
  A new turn never starts while the previous one is still finishing its interrupt.
- **Barge-in** (`codex.barge_in`, default `steer`): calling `respond()` while a Codex turn is still producing
  output sends `turn/steer {threadId, expectedTurnId, input}`; the running turn's further output goes to the
  new `Turn` (deltas of the pre-steer answer are dropped until the steer is consumed) and the old `Turn` ends
  `Cancelled` with `timings.superseded = true`. The turn is claimed synchronously inside `respond()`, so simply
  replacing/dropping the old `Turn` does not interrupt it first. If the turn ends before it consumes the steer,
  the text is answered as a new turn. `interrupt`: the running turn gets `turn/interrupt`, then a new turn
  starts on the same thread. An explicit `cancel()` (or an empty line in `chat`) still interrupts.
- Post-interrupt latency: measured and traced (RUST_LOG): `turn/interrupt` makes the app-server drop its
  Responses WebSocket, so the next turn reconnects (~2 s here) and resends without the cached response chain,
  costing ~3-6 s once; the turn after is normal. Not fixable from the client: overriding
  `supports_websockets` is ignored for the built-in provider, an HTTP-only provider is slower on every turn
  (5-7 s), and `thread/sample` warm-up needs persistent threads and takes ~13 s. Steering (default barge-in)
  avoids the interrupt entirely; in dual mode HTTP covers a slow Codex turn after 2 s.
- `lean: true`: this child runs with `-c features.plugins=false -c features.apps=false` and
  `-c mcp_servers.<name>.enabled=false` for your MCP servers (only header names are read from config.toml;
  the file is never modified). Built-in shell/file tools stay.
- Child death: every pending call fails, turn streams close, engine marked unhealthy, respawn after 10 s
  (the conversation thread is lost then; a new one is started).

## HTTP engine changes

- All routes start at **t=0** (`http_race_at_start: true`); set false for the old stagger by `first_token_timeout_ms`.
- DeepSeek Flash now sends `thinking: {type: disabled}` (it used to burn the 200-token budget thinking and return
  nothing; now TTFT ≈ 0.3-0.8 s). Zero-width-only tokens can't win a race (DeepSeek once streamed only those).

## Direct network for GLM / DeepSeek / Qwen (`Config.http_network`, `src/net.rs`)

Same approach as esp-bot's "通过活动 Wi-Fi 或有线网卡直连" (`docs/macos-active-interface.md`; reimplemented, nothing
copied): pick the active Wi-Fi/Ethernet interface from macOS's service order (`scutil`: AirPort/Ethernet, link not
down, router + address), resolve the provider host with the system resolver plus that service's DNS servers
(A + AAAA, VPN fake-IP 198.18.0.0/15 and non-global answers dropped), bind every socket to the interface
(`IP_BOUND_IF`/`IPV6_BOUND_IF` via reqwest `.interface()`), `no_proxy`, `https_only`, TLS checked against the
provider hostname. With several addresses, TLS handshakes are raced on the bound interface (no HTTP sent) and the
winner is put first, cached 5 min. Interface re-checked every 10 s. No system route changes.

- `http_network`: **`auto`** (default: direct when an active interface exists, else the system path),
  `active-interface` (fail the attempt without one), `system` (the old behaviour; follows the VPN).
  Env override `XIAODAI_HTTP_NETWORK=system|active-interface|auto`.
- `Route.direct`: true for the built-in GLM / DeepSeek / Qwen routes, false (default) for custom routes.
- New optional route `glm-coding-plan-cn` (`open.bigmodel.cn`, the endpoint esp-bot switched to on 11-18; same
  key variable, works with the current key). Not in the default order.
- On this Mac (VPN on `utun6` with 0/1 + 128/1 routes) the VPN client already bypasses `api.z.ai` and
  `api.deepseek.com` (both route via `en0` on the system path, same address), so "system" vs "direct" there is
  mostly the same wire path; `open.bigmodel.cn` IPv4 goes through `utun6` on the system path, and direct uses
  its IPv6 addresses on `en0` (its IPv4 addresses are not reachable bound to `en0`).
- Qwen coding plan answers **HTTP 405 "Coding Plan is currently only available for Coding Agents"** on both
  paths: the plan only serves coding-agent clients (esp-bot reaches it through OpenCode). Not worked around.

## Latency log + Luna-first fallback (`Config.health`, `src/health.rs`)

Every attempt the core starts (Luna, each HTTP route, background probes) ends as one sample: time to first visible
text from its own start, or `lost` (another engine spoke first), `failed`, `timeout`. Samples are kept in a
rolling window per engine ("codex" = Luna, else the route id) and appended as JSON lines to
`health.log_path` (default `<crate>/runtime/latency.jsonl`, `XIAODAI_LATENCY_LOG` overrides, empty = memory
only; rotated to `.1` past 5 MiB; reloaded at startup, samples older than 10 min ignored). Switches are logged too.

Dual mode only. **While Luna is healthy nothing changes**: Luna at t=0, HTTP after `codex.fallback_after_ms`,
first visible text wins (covered by a fake-server test that checks the plan and that HTTP is never contacted).
Luna becomes *degraded* when
- unavailable: `fail_threshold` (2) failures/timeouts in a row, or a failure streak lasting `unavailable_for_ms`
  (30 s); or
- slow: over the last `window` (6) Luna turns (at least `min_samples` = 3), more than half took longer than
  `slow_ttft_ms` (3000) to speak or lost the race.

While degraded: the HTTP routes lead at t=0, fastest recent route first (median of the window; failures count as
30 s, a lost race as its lower bound + 50 ms; routes without data keep config order). Luna is kept only as a
**failure-only last resort** (started when every HTTP route has failed). A background probe sends one tiny turn
(`健康检查：只回复一个字“好”。`) on a separate throwaway Codex thread every `probe_interval_ms` (20 s; ceiling
`probe_timeout_ms` 20 s; the conversation thread never sees probes; a dead child is respawned with the usual
backoff); `recover_probes` (2) probes in a row under `slow_ttft_ms` switch back to Luna. `health.enabled = false`
keeps the log but never leaves Luna. Note: user turns answered by HTTP while degraded are not in Luna's thread.

## Grok Bot delegation (`CodexConfig.grok_bot`)

Grok Bot is a **delegate of Luna, not a racing engine**. When enabled (default: on if
`~/grok-bot/mcp/server.mjs` exists; `XIAODAI_GROK_BOT=0|off|false` turns it off) the core's own codex child gets
`-c mcp_servers.xiaodai_grok_bot.{command,args,enabled,startup_timeout_sec,tool_timeout_sec,default_tools_approval_mode,tools.<tool>.approval_mode}`
overrides (node + server.mjs; added after the lean set, so other user MCP servers stay off; `~/.codex/config.toml`
is never read for values or written). Tools: `ask_grok_bot` (submit, returns `request_id`), `get_grok_bot_result`,
`ask_grok_bot_and_wait`. The developer instructions gain a short note: answer simple things directly; for heavier
read-only work (web research, weather, places/opening hours, X posts, URL summaries, translation) submit with
`ask_grok_bot`, say in one sentence that Grok Bot has it, and check with `get_grok_bot_result` when asked; the
CLI (`~/grok-bot/cli/grokbot --no-wait "任务"` / `--result <id>`) is mentioned as a fallback; Grok Bot does not send
messages, buy, book, fill forms or change files. An MCP elicitation is accepted only from this server (every other
server's is declined). The server reads its own webhook credentials; nothing secret is passed by the core.

## Run

```sh
cargo test
cargo run --release --bin fast-bench                 # full live bench: codex-only / http-only / dual + interrupts + workspace test
cargo run --release --bin fast-bench -- codex        # Codex cold start + model list
cargo run --release --bin fast-bench -- once 用一句话介绍珀斯 [--mode codex-only|http-only|dual]
cargo run --release --bin fast-bench -- conv         # Codex: same-thread context, steer, post-interrupt TTFT
cargo run --release --bin fast-bench -- chat         # typing while it answers = barge-in; empty line/Ctrl-C interrupts
BENCH_MODES=codex-only BENCH_QUICK=1 cargo run --release --bin fast-bench -- bench --codex-tier priority
BENCH_N=8 BENCH_ROUTES=glm-coding-plan,deepseek-flash cargo run --release --bin fast-bench -- net  # system vs direct
cargo run --release --bin fast-bench -- fallback     # simulated Luna outage -> HTTP leads -> probe switches back
cargo run --release --bin fast-bench -- delegate     # ONE harmless Luna -> Grok Bot round trip (sends a real task)
cargo run --release --bin fast-bench -- health       # rolling latency stats + Luna state
```
Bench runs write their latency log and scratch files only under `~/Xiaodai_Server/runtime/tmp/xiaodai-core-test`
(unless `XIAODAI_LATENCY_LOG` is set); tests use no log file (one unit test writes there and removes its folder).
Options: `--codex-model`, `--codex-effort`, `--codex-tier`, `--fallback-ms`, `--barge-in steer|interrupt`, `--env-file`. Keys come from env or `.env`;
key values, tokens and ~/.codex auth contents are never read, printed or logged.

`.cargo/config.toml` sets `DEVELOPER_DIR=/Library/Developer/CommandLineTools` (Xcode license not accepted).

## Results (2026-11-18, Perth, afternoon run)

Tests: **43 passed** (16 unit; 16 fake app-server over pipes: stream, interrupt, approvals, unknown request,
crash→HTTP fallback, turn failure, not logged in, missing model, dual timing both ways, **one thread across
turns + interrupts with context kept, next turn waits for the interrupted turn, steer takes over the running
turn with `expectedTurnId` and no stale tail, steer survives dropping the old Turn, interrupt policy, steer
not consumed → new turn**; 11 HTTP mock).

Codex cold start (0.169 + static catalog): spawn→initialized 90-150 ms, account/read 3.4-3.5 s, model/list 1-2 ms,
thread/start 71-73 ms, priming turn 9.8-16 s, total 13-20 s (once at startup).

| prompt | codex-only | http-only (winner deepseek) | dual (winner) |
|---|---|---|---|
| 你好 | 1.75 s | 0.76 s | 2.99 s deepseek |
| 现在几点适合喝咖啡？ | 12.7 s (ran a tool) | 0.52 s | 2.62 s deepseek |
| 用一句话介绍珀斯 | 5.86 s | 0.63 s | 2.26 s codex |
| 讲个很短的笑话 | 1.92 s | 0.87 s | 1.64 s codex |

(time to first visible text; the Mac's network was slow and variable during this run.)

Conversation bench (`fast-bench conv`, codex-only, two runs):
- Same thread: 我叫小王。→ 用一句话介绍珀斯 → 我叫什么？ answered "你叫小王。" both runs; one thread for the whole
  session (threads started: 1) including after interrupts and steers.
- Steer (follow-up "改成讲一只狗，只说一句话。" after the first sentence of a cat story): `turn/steer` ack
  1-2 ms; first token of the steered answer 2.1-5.9 s after the steer; all 4 answers switched to a dog story
  (two of them still mention a kitten inside it); old Turn ended `Cancelled`, `superseded = true`.
- After an interrupt: normal turn 2.2-2.7 s; next turn after the interrupt 6.0-10.6 s; the turn after that
  1.9-4.2 s (see "Post-interrupt latency" above).

Interrupt (cancel 300 ms after first token): no more output after 0.03-0.14 ms, 0 sentences after cancel;
`turn/interrupt` sent in ≤0.01 ms, `turn/completed (interrupted)` 1-3 ms after cancel.
Workspace: Codex created `runtime/tmp/xiaodai-core-test/hello.txt` with "小呆你好" (removed afterwards); a write
to `~/temp` (outside the root) was blocked by the sandbox.

## Results (2026-11-18, Perth, 15:01-15:12 AWST): direct network, fallback, delegation

Tests: **61 passed** (26 unit, 24 fake app-server, 11 HTTP mock); clippy clean (`--all-targets`). New fake-server
tests: healthy Luna keeps the old dual plan (HTTP never contacted) and exposes rolling stats in `Timings`; Luna
outage → immediate HTTP failover → degraded after 2 failures → HTTP leads at t=0 with Luna not started → probe
thread separate from the conversation → 2 good probes switch back → Luna leads again; slow Luna (lost races)
degrades and the fastest recent route leads; degraded Luna is the last resort when every HTTP route fails, but is
not started while an HTTP route is still running; disabled policy never leaves Luna; Grok Bot `-c` overrides and
the instructions note appear only when enabled; elicitations accepted only from the delegate server. Unit tests:
failure count / failure duration / slow window / probe recovery / ranking / log round trip.

Direct vs system path, HTTP-only, warm connections, alternating (TTFT ms, time to first visible text):

| route | path | run 1 (n=6) | median | run 2 (n=8) | median |
|---|---|---|---|---|---|
| glm-coding-plan (api.z.ai) | system | 1502, 2104, 6906, 2149, 1615, 1261 | 2104 | 1676, 1747, 9025, 2165, 1917, 1865, 6304, 3798 | 2165 |
| glm-coding-plan (api.z.ai) | direct en0 | 23898, 1149, 5535, 1194, 1109, 1445 | 1445 | 3046, 1046, 1463, 1857, 1970, 1302, 2507, 3713 | 1970 |
| glm-coding-plan-cn (open.bigmodel.cn) | system (utun6) | 1854, 1474, 1635, 6244, 1910, 1804 | 1854 | 10122, 5489, 1619, 11887, 2413, 2242, 1738, 2964 | 2964 |
| glm-coding-plan-cn (open.bigmodel.cn) | direct en0 (IPv6) | 1582, 1581, 4148, 1635, 4162, 1700 | 1700 | 2516, 3170, 7569, 3304, 3487, 1483, 5736, 1501 | 3304 |
| deepseek-flash | system | 561, 466, 452, 674, 478, 853 | 561 | 483, 952, 617, 818, 884, 880, 422, 687 | 818 |
| deepseek-flash | direct en0 | 816, 507, 833, 543, 560, 963 | 816 | 643, 676, 668, 800, 636, 605, 549, 609 | 643 |
| qwen-coding-plan | both | HTTP 405 every attempt (coding agents only) | - | not rerun | - |

No consistent winner: GLM's own variance (1-24 s) dominates; DeepSeek is within ~±250 ms either way. The direct
path did not fail once. One direct GLM turn took 23.9 s (first token), the system path also had 6.9-9.0 s turns.

Simulated Luna outage (`fallback`: codex binary = wrapper that exits until a flag file exists; probe every 3 s,
respawn backoff 2 s): outage turns 1-2 Luna failed at once, HTTP answered (633 / 514 ms); switched to "fastest"
3.5 s after start (2 failures); turns 3-4 HTTP led at t=0 without Luna, deepseek-flash first (779 / 647 ms).
After restoring Luna, the probe switched back **17.5 s** later (includes codex cold start + probe-thread priming;
second probe 1269 ms). Recovered turn 1: Luna answered in 1112 ms; turns 2-3 normal dual (HTTP after 2 s won).

Grok Bot delegation, ONE round trip (`delegate`, codex-only, cold start 8.0 s not included): Luna called
`ask_grok_bot_and_wait` to translate "good morning"; spoken answer "Grok Bot 的译文是：早上好。" first text at
**14.66 s**, done 15.00 s; inbox `req-16321531.json` status done, result 早上好; no approval/elicitation needed.
Luna codex-only TTFT with delegation on vs off (one run each, 4 prompts): 1088/2431/843/972 ms vs
918/2789/1604/1935 ms, cold start 5964 vs 5915 ms: no measurable cost. Dual mode with delegation on (healthy):
1532/2639/1005/915 ms, Luna won all four.
