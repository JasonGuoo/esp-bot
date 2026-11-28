server_tests \
    brain::fast_core::tests::a_busy_luna_lane_has_a_deadline_and_semantic_admission_is_bounded \
    brain::fast_core::tests::tool_tasks_private_scope_and_pending_inputs_stay_on_opencode
cargo test --offline --locked --manifest-path server/xiaodai-core/Cargo.toml --test mock_server stateless_fallback_never_touches_luna_or_widens_route_policy
