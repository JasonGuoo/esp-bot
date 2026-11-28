// Device runtime and gateway run in separate processes over a host device adapter.
use super::*;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

async fn step(input: &mut ChildStdin, output: &mut BufReader<ChildStdout>, command: &str) -> Value {
    assert!(!command.contains(['\n', '\r']));
    input
        .write_all(format!("{command}\n").as_bytes())
        .await
        .unwrap();
    let mut line = String::new();
    assert!(
        output.read_line(&mut line).await.unwrap() > 0,
        "simulator exited at {command}"
    );
    println!("{command} → {}", line.trim_end());
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
async fn host_bot_completes_clock_turn_and_rejects_late_reply() {
    timeout(Duration::from_secs(60), clock_scenario())
        .await
        .expect("simulator deadline exceeded");
}

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

type SimSocket = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;
struct Rig {
    brain: Arc<Brain>,
    socket: SimSocket,
    child: tokio::process::Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
    _scratch: Scratch,
}
impl Rig {
    async fn new() -> Self {
        Self::with_model_mode("http-only").await
    }
    async fn with_model_mode(mode: &str) -> Self {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let mut child = tokio::process::Command::new(repo.join("tools/sim-bot"))
            .arg("shell")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap());
        let scratch =
            Scratch(std::env::temp_dir().join(format!("espbot-sim-{}", uuid::Uuid::new_v4())));
        let tls = scratch.0.join("tls");
        let generated = std::process::Command::new("python3")
            .arg(repo.join("server/bot-server/scripts/create_gateway_certificate.py"))
            .arg("--directory")
            .arg(&tls)
            .args(["--ip", "127.0.0.1"])
            .output()
            .unwrap();
        assert!(
            generated.status.success(),
            "{}",
            String::from_utf8_lossy(&generated.stderr)
        );
        let mut config = crate::brain::settings::BrainConfig::default();
        config.brain_core.mode = mode.into();
        config.gateway.enabled = true;
        config.gateway.bind = "127.0.0.1:0".into();
        config.gateway.certificate_pem = Some(tls.join("gateway-cert.pem"));
        config.gateway.private_key_pem = Some(tls.join("gateway-key.pem"));
        crate::brain::settings::save(&scratch.0, &config).unwrap();
        let brain = Brain::new(scratch.0.clone(), None, Arc::new(|_| {})).unwrap();
        *brain.gateway.lock().await =
            Some(crate::brain::gateway::start(brain.clone()).await.unwrap());
        brain
            .accepting
            .store(true, std::sync::atomic::Ordering::Release);
        let mut socket = connect_peer(&brain, "sim-bot").await;
        let welcome = socket.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(welcome.to_text().unwrap()).unwrap()["device_id"],
            "sim-bot"
        );

        Self {
            brain,
            socket,
            input,
            output,
            child,
            _scratch: scratch,
        }
    }
    async fn event(&mut self, command: &str) -> Value {
        step(&mut self.input, &mut self.output, command).await
    }
    async fn finish(mut self) {
        self.socket.close(None).await.unwrap();
        self.brain.stop().await.unwrap();
        drop(self.input);
        assert!(self.child.wait().await.unwrap().success());
    }
}
async fn clock_scenario() {
    let mut rig = Rig::new().await;
    let passive = rig.event("sound").await;
    assert_eq!(passive["runtime"]["session_id"], 0);
    assert_eq!(passive["uplink"], false);
    for event in ["link", "audio-ready", "touch"] {
        rig.event(event).await;
    }
    let listening = rig.event("tick 1200").await;
    assert_eq!(listening["conversation"], "listening");
    let token = &listening["runtime"];
    let opened = send_request(&mut rig.socket, "open", "session.open", &json!({
        "activation":"touch","runtime":token,"output":{"version":1,"audio":false,"cards":["text"]}
    })).await;
    assert_eq!(opened["ok"], true, "{opened}");
    assert_eq!(rig.event("onset").await["uplink"], true);
    rig.event("end").await;
    rig.socket
        .send(Message::Text(
            json!({"v":3,"type":"request","id":"clock","method":"assistant.text",
        "payload":{"text":"现在几点","runtime":token}})
            .to_string(),
        ))
        .await
        .unwrap();
    let mut accepted = false;
    let answer = loop {
        let message = rig.socket.next().await.unwrap().unwrap();
        let Message::Text(text) = message else {
            continue;
        };
        let frame: Value = serde_json::from_str(&text).unwrap();
        if frame["id"] != "clock" {
            continue;
        }
        if frame["event"] == "turn.accepted" {
            assert_eq!(&frame["runtime"], token);
            assert_eq!(rig.event("thinking").await["conversation"], "thinking");
            accepted = true;
        } else if frame["type"] == "response" {
            assert!(accepted && frame["ok"] == true, "{frame}");
            assert_eq!(&frame["runtime"], token);
            break frame["payload"]["scene"]["speech"]
                .as_str()
                .unwrap()
                .to_owned();
        }
    };
    assert!(answer.contains("现在是"), "{answer}");
    let reply = format!("reply {answer}");
    let waiting = rig.event(&reply).await;
    assert_eq!(waiting["reading"], answer);
    assert_eq!(waiting["followup"], true);
    let stopped = rig.event("stop").await;
    assert_eq!(stopped["runtime"]["session_id"], 0);
    assert_eq!(
        send_request(&mut rig.socket, "close", "session.close", &json!({})).await["ok"],
        true
    );
    let late = rig.event(&reply).await;
    assert_eq!(
        late["rejected"].as_u64(),
        stopped["rejected"].as_u64().map(|n| n + 1)
    );
    assert_eq!(late["reading"], "");
    let denied = send_request(
        &mut rig.socket,
        "late",
        "assistant.text",
        &json!({"text":"现在几点","runtime":token}),
    )
    .await;
    assert_eq!(denied["error"]["code"], "explicit_session_required");
    rig.finish().await;
}

#[tokio::test]
async fn host_bot_script_handles_time_boundaries_and_invalid_commands() {
    use std::io::Write;
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let mut child = std::process::Command::new(repo.join("tools/sim-bot"))
        .arg("shell")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"tick 1\ntick 600000\ntick 600001\ntouch\n")
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert_eq!(result.status.code(), Some(2));
    let snapshots: Vec<Value> = std::str::from_utf8(&result.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0]["ms"], 1001);
    assert_eq!(snapshots[1]["ms"], 601001);
    assert_eq!(snapshots[1]["runtime"]["session_id"], 0);
}

// The inference boundary can withhold a reply until a control operation has
// completed. Receiving the prompt is the synchronization point (no sleep).
struct Model {
    requests: tokio::sync::mpsc::Receiver<(Value, tokio::sync::oneshot::Sender<String>)>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Model {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Model {
    async fn start(rig: &Rig) -> Self {
        let (tx, requests) =
            tokio::sync::mpsc::channel::<(Value, tokio::sync::oneshot::Sender<String>)>(4);
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let tx = tx.clone();
                async move {
                    let (release, wait) = tokio::sync::oneshot::channel();
                    tx.send((body, release)).await.unwrap();
                    let text = wait.await.unwrap_or_default();
                    let delta =
                        json!({"choices":[{"delta":{"content":text},"finish_reason":"stop"}]});
                    (
                        [("content-type", "text/event-stream")],
                        format!("data: {delta}\n\ndata: [DONE]\n\n"),
                    )
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        rig.brain
            .fast
            .use_loopback_model(listener.local_addr().unwrap().port());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self { requests, server }
    }
    async fn pending(&mut self, user: &str) -> (String, tokio::sync::oneshot::Sender<String>) {
        let (body, release) = timeout(Duration::from_secs(5), self.requests.recv())
            .await
            .expect("model never received prompt")
            .unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.last().unwrap()["content"], user);
        (
            messages
                .iter()
                .filter_map(|m| m["content"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            release,
        )
    }
}

impl Rig {
    async fn open_chat(&mut self, live: bool) -> (Value, String) {
        for event in ["link", "audio-ready", "touch"] {
            self.event(event).await;
        }
        let token = self.event("tick 1200").await["runtime"].clone();
        let mut output = json!({"version":1,"cards":["text"],"audio":false});
        if live {
            output["presentation"] = json!({"version":1,"frame_ack":true});
            output["live_turns"] = json!(true);
        }
        assert!(
            token["session_id"].as_u64().unwrap() > 0,
            "touch did not activate runtime"
        );
        let request_id = format!("open-{}", token["epoch"]);
        self.send(
            &request_id,
            "session.open",
            json!({"runtime":token,"activation":"touch","output":output}),
        )
        .await;
        let open = loop {
            let frame = self.frame().await;
            if frame["id"] == request_id {
                break frame;
            }
            assert_ne!(frame["type"], "response", "unexpected response: {frame}");
            assert_ne!(
                frame["event"], "presentation.segment",
                "unexpected segment: {frame}"
            );
        };
        assert_eq!(open["ok"], true, "{open}");
        (
            token,
            open["payload"]["conversation_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    }
    async fn send(&mut self, id: &str, method: &str, payload: Value) {
        self.socket
            .send(Message::Text(
                json!({"v":3,"type":"request","id":id,"method":method,"payload":payload})
                    .to_string(),
            ))
            .await
            .unwrap();
    }
    async fn frame(&mut self) -> Value {
        loop {
            if let Message::Text(text) = self.socket.next().await.unwrap().unwrap() {
                return serde_json::from_str(&text).unwrap();
            }
        }
    }
    async fn ask(&mut self, id: &str, text: &str, token: &Value) {
        let onset = self.event("onset").await;
        assert_eq!(onset["uplink"], true);
        assert_eq!(&onset["runtime"], token);
        self.event("end").await;
        self.send(id, "assistant.text", json!({"text":text,"runtime":token}))
            .await;
        loop {
            let frame = self.frame().await;
            if frame["id"] == id {
                assert_eq!(frame["event"], "turn.accepted", "{frame}");
                assert_eq!(&frame["runtime"], token);
                break;
            }
            assert_ne!(
                frame["event"], "presentation.segment",
                "unexpected content: {frame}"
            );
        }
        assert_eq!(self.event("thinking").await["conversation"], "thinking");
    }
    async fn interrupt(&mut self, method: &str, token: &Value) {
        self.send("cut", method, json!({"runtime":token})).await;
        let mut cancelled = false;
        loop {
            let f = self.frame().await;
            if f["type"] == "response" && f["id"] == "old" {
                assert_eq!(f["error"]["code"], "cancelled", "{f}");
                assert_eq!(&f["runtime"], token);
                cancelled = true;
            }
            if f["id"] == "cut" {
                assert!(cancelled && f["payload"]["cancelled"] == true, "{f}");
                return;
            }
            assert_ne!(
                f["event"], "presentation.segment",
                "unheard content leaked: {f}"
            );
        }
    }
    async fn receive_answer(
        &mut self,
        id: &str,
        token: &Value,
        expected: &str,
        live: bool,
    ) -> Value {
        let mut responded = false;
        let mut segment = None;
        let mut displayed = String::new();
        while !responded || (live && segment.is_none()) {
            let frame = self.frame().await;
            if frame["type"] == "response" {
                assert_eq!(frame["id"], id, "late response: {frame}");
                assert_eq!(frame["ok"], true, "{frame}");
                assert_eq!(&frame["runtime"], token);
                responded = true;
                if !live {
                    displayed = frame["payload"]["scene"]["speech"]
                        .as_str()
                        .unwrap()
                        .to_owned();
                }
            }
            if frame["event"] == "presentation.segment" {
                assert!(live);
                assert_eq!(&frame["runtime"], token, "late segment: {frame}");
                displayed = frame["payload"]["scene"]["speech"]
                    .as_str()
                    .unwrap()
                    .to_owned();
                assert_eq!(frame["payload"]["final"], true);
                segment = Some(frame["payload"].clone());
            }
        }
        assert_eq!(displayed, expected);
        let seen = self.event(&format!("reply {displayed}")).await;
        assert_eq!(seen["reading"], expected);
        if let Some(mut report) = segment {
            report["runtime"] = token.clone();
            report["status"] = json!("completed");
            assert_eq!(
                send_request(
                    &mut self.socket,
                    "report",
                    "presentation.segment.report",
                    &report
                )
                .await["ok"],
                true
            );
        }
        assert_eq!(seen["followup"], true);
        seen["runtime"].clone()
    }
}

const STORY: &str = "给我讲一个小兔的故事";
const CHANGE: &str = "补一句，把小兔换成小狗";
const OLD: &str = "小兔跳过小溪。";
const NEW: &str = "小狗跳过小溪。";

#[tokio::test]
async fn slow_supplement_keeps_question_and_retires_unheard_answer() {
    timeout(Duration::from_secs(40), async {
        let mut rig = Rig::new().await;
        let mut model = Model::start(&rig).await;
        let (old, _) = rig.open_chat(true).await;
        rig.ask("old", STORY, &old).await;
        let (_, release_old) = model.pending(STORY).await;
        assert_eq!(rig.event("tick 2200").await["conversation"], "thinking");
        let cut = rig.event("supplement").await;
        let next = &cut["runtime"];
        assert_eq!(cut["supplement"], true);
        assert_eq!(next["session_id"], old["session_id"]);
        assert_eq!(
            next["turn_id"].as_u64(),
            old["turn_id"].as_u64().map(|n| n + 1)
        );
        rig.interrupt("assistant.supplement", &old).await;
        // The upstream may already have observed cancellation.
        let _ = release_old.send(OLD.into());
        assert_eq!(rig.event(&format!("reply {OLD}")).await["rejected"], 1);
        rig.ask("new", CHANGE, next).await;
        let (prompt, release) = model.pending(CHANGE).await;
        assert!(
            prompt.contains(&format!("\"上次问题\":\"{STORY}\""))
                && prompt.contains("\"已播整句\":\"\""),
            "{prompt}"
        );
        assert!(!prompt.contains(OLD));
        release.send(NEW.into()).unwrap();
        rig.receive_answer("new", next, NEW, true).await;
        rig.finish().await;
    })
    .await
    .expect("supplement deadline");
}

#[tokio::test]
async fn slow_stop_is_acknowledged_before_provider_finishes_and_next_wake_works() {
    timeout(Duration::from_secs(40), async {
        let mut rig = Rig::new().await;
        let mut model = Model::start(&rig).await;
        let (old, _) = rig.open_chat(false).await;
        rig.ask("old", STORY, &old).await;
        let (_, release_old) = model.pending(STORY).await;
        assert_eq!(rig.event("tick 2200").await["conversation"], "thinking");
        let stopped = rig.event("stop").await;
        assert_eq!(stopped["uplink"], false);
        assert_eq!(stopped["runtime"]["session_id"], 0);
        rig.interrupt("session.close", &old).await;
        let _ = release_old.send(OLD.into());
        let late = rig.event(&format!("reply {OLD}")).await;
        assert_eq!(late["rejected"], 1);
        assert_eq!(late["reading"], "");
        let (fresh, _) = rig.open_chat(false).await;
        assert_ne!(old["session_id"], fresh["session_id"]);
        rig.ask("fresh", "今天聊什么", &fresh).await;
        let (prompt, reply) = model.pending("今天聊什么").await;
        assert!(!prompt.contains("用户在补充上一个问题") && !prompt.contains(OLD));
        reply.send("我们聊聊星星。".into()).unwrap();
        rig.receive_answer("fresh", &fresh, "我们聊聊星星。", false)
            .await;
        rig.finish().await;
    })
    .await
    .expect("stop deadline");
}

#[tokio::test]
async fn running_background_task_allows_two_chat_turns_and_retains_its_result() {
    use crate::brain::tasks::{TaskResult, TaskState};
    use tokio::sync::Notify;
    timeout(Duration::from_secs(40), async {
        let mut rig = Rig::new().await;
        let mut model = Model::start(&rig).await;
        // Only this disposable household opts in to using prior turns as model context.
        rig.brain
            .memory
            .update_household_settings(&json!({"history_enabled":true,"allow_cloud":true}))
            .unwrap();
        let (mut token, conversation) = rig.open_chat(false).await;
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let (enter, gate) = (started.clone(), release.clone());
        rig.brain
            .tasks
            .register_handler(
                "agent.work",
                Arc::new(move |_| {
                    let (enter, gate) = (enter.clone(), gate.clone());
                    Box::pin(async move {
                        enter.notify_one();
                        gate.notified().await;
                        Ok(TaskResult {
                            state: TaskState::Succeeded,
                            summary: "资料已经整理好了。".into(),
                            artifact_ids: vec![],
                            data: json!({}),
                        })
                    })
                }),
            )
            .unwrap();
        // Give the pre-existing job its own source, distinct from both chat turns.
        let source = rig
            .brain
            .memory
            .journal_input(&crate::memory::JournalInput {
                turn_id: "delegated-research".into(),
                activation_id: conversation.clone(),
                utterance_id: None,
                text: "帮我查资料".into(),
                occurred_at_ms: crate::time::now_ms() as i64,
                input_kind: "text".into(),
                speaker_person_id: None,
                private_profile_id: None,
                allow_cloud: false,
                metadata: json!({}),
            })
            .unwrap();
        let task = rig
            .brain
            .tasks
            .submit(
                crate::brain::tasks::NewTask {
                    household_id: source.household_id,
                    daily_conversation_id: source.daily_conversation_id,
                    conversation_turn_id: source.conversation_turn_id,
                    capability: "agent.work".into(),
                    input: json!({"prompt":"查资料","activation_id":conversation}),
                    intent_key: "sim-research".into(),
                },
                false,
            )
            .unwrap()
            .task_id;
        rig.brain.tasks.start().await.unwrap();
        started.notified().await;
        for (id, question, response) in [
            ("chat1", "我们聊聊星星好吗", "好呀，我们抬头看看星星。"),
            ("chat2", "它为什么发光", "因为它很热。"),
        ] {
            rig.ask(id, question, &token).await;
            let (prompt, answer) = model.pending(question).await;
            if id == "chat2" {
                assert!(
                    prompt.contains("我们聊聊星星好吗"),
                    "lost earlier chat context: {prompt}"
                );
            }
            answer.send(response.into()).unwrap();
            token = rig.receive_answer(id, &token, response, false).await;
            assert_eq!(
                rig.brain.tasks.get(&task).unwrap().state,
                TaskState::Running
            );
        }
        release.notify_one();
        loop {
            if rig.brain.tasks.get(&task).unwrap().state == TaskState::Succeeded {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            rig.brain.tasks.inbox_state(&task).unwrap().as_deref(),
            Some("pending_delivery")
        );
        assert_eq!(rig.brain.tasks.attempts(&task).unwrap().len(), 1);
        assert_eq!(
            rig.brain.tasks.get(&task).unwrap().result["summary"],
            "资料已经整理好了。"
        );
        // Actual socket observation after the durable result, through a round trip.
        rig.send("barrier", "proactive.ready", json!({})).await;
        loop {
            let f = rig.frame().await;
            if f["id"] == "barrier" {
                assert_eq!(f["ok"], true);
                break;
            }
            assert_ne!(
                f["event"], "presentation.segment",
                "unsolicited task output: {f}"
            );
            assert_ne!(f["event"], "proactive.offer", "unsolicited task offer: {f}");
        }
        assert_eq!(rig.event("state").await["reading"], "因为它很热。");
        rig.finish().await;
    })
    .await
    .expect("background chat deadline");
}

#[tokio::test]
async fn stop_between_supplement_and_capture_closes_only_the_reserved_turn() {
    timeout(Duration::from_secs(40), async {
        let mut rig = Rig::new().await;
        let mut model = Model::start(&rig).await;
        let (old, _) = rig.open_chat(true).await;
        rig.ask("old", STORY, &old).await;
        let (_, release_old) = model.pending(STORY).await;
        rig.event("tick 2200").await;
        let next = rig.event("supplement").await["runtime"].clone();
        rig.interrupt("assistant.supplement", &old).await;
        let _ = release_old.send(OLD.into());
        let stopped = rig.event("stop").await;
        assert_eq!(stopped["runtime"]["session_id"], 0);
        assert_eq!(stopped["uplink"], false);

        // brain_secure switches its remote token after sending assistant.supplement
        // and can send session.close before its next iteration sends audio.begin.
        for field in ["session_id", "turn_id", "epoch", "focus_generation"] {
            let mut unrelated = next.clone();
            unrelated[field] = json!(next[field].as_u64().unwrap() + 1);
            let id = format!("wrong-{field}");
            let rejected = send_request(
                &mut rig.socket,
                &id,
                "session.close",
                &json!({"runtime":unrelated}),
            )
            .await;
            assert_eq!(rejected["error"]["code"], "stale_runtime", "{rejected}");
        }
        let close = send_request(
            &mut rig.socket,
            "stop-before-capture",
            "session.close",
            &json!({"runtime":next}),
        )
        .await;
        assert_eq!(close["payload"]["cancelled"], true, "{close}");
        let late_input = send_request(
            &mut rig.socket,
            "late-input",
            "assistant.text",
            &json!({"runtime":next,"text":CHANGE}),
        )
        .await;
        assert_eq!(
            late_input["error"]["code"], "explicit_session_required",
            "{late_input}"
        );
        let (fresh, _) = rig.open_chat(false).await;
        assert_ne!(fresh["session_id"], next["session_id"]);
        let late_close = send_request(
            &mut rig.socket,
            "late-close",
            "session.close",
            &json!({"runtime":next}),
        )
        .await;
        assert_eq!(late_close["error"]["code"], "stale_runtime", "{late_close}");
        rig.ask("fresh", "今天聊什么", &fresh).await;
        let (prompt, reply) = model.pending("今天聊什么").await;
        assert!(!prompt.contains("用户在补充上一个问题") && !prompt.contains(STORY));
        reply.send("我们聊聊星星。".into()).unwrap();
        rig.receive_answer("fresh", &fresh, "我们聊聊星星。", false)
            .await;
        rig.finish().await;
    })
    .await
    .expect("stop at supplement handover deadline");
}

mod child_dialogues {
    include!("child_dialogues.rs");
}
