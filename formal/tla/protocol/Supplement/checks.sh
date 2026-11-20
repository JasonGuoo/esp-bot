server_tests brain::gateway::tests::runtime_leases_advance_without_reopening_the_conversation \
    voice_context_uses_only_completed_speech supplement_uses_the_previous_question
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$repo_dir/server/bot-server/src-tauri/target}"
rust_tests server/xiaodai-core/Cargo.toml --test=codex_fake \
    followup_steers_the_running_turn_and_takes_over_its_output fresh_question_cannot_steer_a_previous_speakers_turn
