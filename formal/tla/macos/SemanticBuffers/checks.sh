cargo test --offline --locked --manifest-path server/xiaodai-core/Cargo.toml --lib -- \
    cancelled_requests_release_their_pending_capacity \
    notification_overflow_closes_the_stream_without_skipping_its_prefix \
    rpc_line_and_outgoing_queues_have_finite_budgets \
    a_stalled_semantic_consumer_gets_one_explicit_overflow_after_its_prefix
cargo test --offline --locked --manifest-path server/xiaodai-core/Cargo.toml --test mock_server -- \
    a_paused_consumer_retains_a_bounded_prefix_and_an_explicit_failure \
    stalled_turns_cannot_spawn_unbounded_races_even_through_core_clones \
    reconfiguration_and_input_limits_are_checked_before_starting_a_provider \
    flushing_the_last_fragment_cannot_exceed_the_sentence_budget
