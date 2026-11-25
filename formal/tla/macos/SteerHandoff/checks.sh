cargo test --offline --locked --manifest-path server/xiaodai-core/Cargo.toml --test codex_fake -- \
    steer_notification_is_buffered_before_ack \
    steer_ack_and_notification_with_no_gap \
    steer_rejected_after_early_output \
    steer_timeout_with_the_original_turn \
    an_older_steer_notification_cannot_clear
