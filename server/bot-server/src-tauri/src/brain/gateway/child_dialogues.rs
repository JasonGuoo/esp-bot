// Speaker labels and review criteria belong to the evaluator. The Bot gets only
// the utterance, just as it would from ASR without a verified speaker identity.
use super::*;
use serde::Deserialize;
use std::io::Write;

#[derive(Deserialize)]
struct FamilyScene {
    id: String,
    title: String,
    turns: Vec<(String, String, String)>,
}

fn scenes() -> Vec<FamilyScene> {
    serde_json::from_str(include_str!(
        "../../../../../../tests/regression/children/scenes.json"
    ))
    .unwrap()
}

fn subject_scenes() -> Vec<FamilyScene> {
    serde_json::from_str(include_str!(
        "../../../../../../tests/regression/children/subjects.json"
    ))
    .unwrap()
}

async fn play(scene: &FamilyScene, live: bool) {
    let mut rig = Rig::with_model_mode(if live { "codex-only" } else { "http-only" }).await;
    let mut model = if live {
        None
    } else {
        Some(Model::start(&rig).await)
    };
    let mut transcript = if live {
        let dir = std::env::var_os("XIAODAI_DIALOGUE_REPORT")
            .expect("use tools/sim-bot children or tools/sim-bot subjects");
        Some(
            std::fs::File::create_new(
                std::path::Path::new(&dir).join(format!("{}.jsonl", scene.id)),
            )
            .unwrap(),
        )
    } else {
        None
    };
    rig.brain
        .memory
        .update_household_settings(&json!({"history_enabled":true,"allow_cloud":true}))
        .unwrap();
    // Only a confirmed private profile could see this synthetic private turn.
    // Give it cloud consent so the household/private boundary itself is tested.
    let private_marker = "椰壳邮局六十七号";
    let sister = rig
        .brain
        .memory
        .create_profile(crate::memory::NewProfile {
            display_name: "阿葵".into(),
            is_child: true,
            age_band: None,
        })
        .unwrap();
    rig.brain
        .memory
        .journal_input(&crate::memory::JournalInput {
            turn_id: "private-backstory".into(),
            activation_id: "private-chat".into(),
            utterance_id: None,
            text: private_marker.into(),
            occurred_at_ms: crate::time::now_ms() as i64,
            input_kind: "text".into(),
            speaker_person_id: None,
            private_profile_id: Some(sister.id),
            allow_cloud: true,
            metadata: json!({}),
        })
        .unwrap();
    let (mut token, _) = rig.open_chat(false).await;
    assert!(scene.turns.len() >= 10);
    for (i, (speaker, user, rubric)) in scene.turns.iter().enumerate() {
        let id = format!("{}-{}", scene.id, i + 1);
        let start = std::time::Instant::now();
        let mut log = |event: Value| {
            if let Some(f) = transcript.as_mut() {
                writeln!(f, "{}", event).unwrap();
                f.flush().unwrap();
            }
        };
        log(
            json!({"event":"asked","id":id,"scene":scene.title,"speaker":speaker,"user":user,"review":rubric}),
        );
        timeout(Duration::from_secs(75), async {
            rig.ask(&id, user, &token).await;
            let accepted_ms = start.elapsed().as_millis();
            if let Some(provider) = model.as_mut() {
                let (prompt, release) = provider.pending(user).await;
                assert!(prompt.contains("本轮输入为纯文字"));
                assert!(
                    !prompt.contains(private_marker),
                    "{id}: private turn in shared prompt"
                );
                if i > 0 {
                    assert!(
                        prompt.contains(&scene.turns[i - 1].1),
                        "{id}: missing preceding user turn"
                    );
                }
                // Preserve the actual missed callbacks. Check that the source
                // facts reach inference, without prescribing a model response.
                if scene.id == "quiet_radio" && i == 8 {
                    assert!(prompt.contains(&scene.turns[6].1));
                }
                if scene.id == "puddle_kitchen" && i == 9 {
                    assert!(prompt.contains(&scene.turns[4].1));
                }
                // Exercises transport only. Live runs never inject an answer.
                release
                    .send(format!("这是第{}轮的测试回包。", i + 1))
                    .unwrap();
            }
            let response = loop {
                let f = rig.frame().await;
                if f["type"] == "response" {
                    assert_eq!(f["id"], id, "{f}");
                    assert_eq!(f["ok"], true, "{f}");
                    assert_eq!(f["runtime"], token);
                    break f;
                }
            };
            let speech = response["payload"]["scene"]["speech"].as_str().unwrap();
            let status = rig.brain.fast.status();
            log(
                json!({"event":"answer","id":id,"answer":speech,"accepted_ms":accepted_ms,"ms":start.elapsed().as_millis(),
                "route":status["last_turn"]["winner"],"outcome":status["last_turn"]["outcome"]}),
            );
            assert!(
                !speech.contains(private_marker),
                "{id}: private turn disclosed"
            );
            if live {
                assert_eq!(
                    status["last_turn"]["winner_engine"], "codex",
                    "{id}: real Luna required"
                );
            }
            assert!(!speech.trim().is_empty(), "{id}: empty answer");
            assert_eq!(status["last_turn"]["outcome"], "answered", "{id}: {status}");
            // The board clips a plain text caption to BOT_CAPTION_MAX_BYTES on a UTF-8 boundary.
            let mut end = speech.len().min(DEVICE_CAPTION_BYTES);
            while !speech.is_char_boundary(end) {
                end -= 1;
            }
            let caption = speech[..end].replace(['\n', '\r'], " ");
            let state = rig.event(&format!("reply {caption}")).await;
            assert_eq!(state["reading"], caption, "{id}");
            assert_eq!(state["followup"], true, "{id}");
            assert_eq!(state["runtime"]["session_id"], token["session_id"]);
            assert_eq!(
                state["runtime"]["turn_id"].as_u64(),
                token["turn_id"].as_u64().map(|n| n + 1)
            );
            token = state["runtime"].clone();
            assert!(
                rig.brain.tasks.list(None, None, 1).unwrap().is_empty(),
                "{id}: unexpected task"
            );
            assert!(
                rig.brain.memory.reminders(1).unwrap().is_empty(),
                "{id}: unexpected reminder"
            );
            log(json!({"event":"displayed","id":id,"caption":caption,"turn":i+1}));
        })
        .await
        .unwrap_or_else(|_| panic!("{id}: timed out waiting for response or display"));
    }
    rig.finish().await;
}

#[tokio::test]
async fn all_child_turns_are_answered_and_reach_the_runtime() {
    for scene in scenes() {
        play(&scene, false).await;
    }
}

#[tokio::test]
async fn all_subject_turns_are_answered_and_reach_the_runtime() {
    for scene in subject_scenes() {
        play(&scene, false).await;
    }
}

macro_rules! live_scenarios {
    ($catalogue:ident, $($test:ident => $id:literal),+ $(,)?) => {
        $(
            #[tokio::test]
            #[ignore = "live model dialogue acceptance"]
            async fn $test() {
                let scene = $catalogue().into_iter().find(|scene| scene.id == $id).unwrap();
                play(&scene, true).await;
            }
        )+
    };
}

live_scenarios!(scenes,
    live_family_moon_ferry => "moon_ferry",
    live_family_quiet_radio => "quiet_radio",
    live_family_sock_detective => "sock_detective",
    live_family_puddle_kitchen => "puddle_kitchen",
);

live_scenarios!(subject_scenes,
    live_subject_math => "math",
    live_subject_english => "english",
    live_subject_chinese => "chinese",
    live_subject_history => "history",
    live_subject_physics => "physics",
    live_subject_biology => "biology",
    live_subject_geography => "geography",
    live_subject_data_music => "data_music",
);
