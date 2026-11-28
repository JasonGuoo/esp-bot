server_tests brain::gateway::jobs::tests brain::gateway::tests::a_locked_task_database_does_not_block_idle_stop_or_status brain::gateway::tests::delayed_cleanup_revokes_only_the_retired_lease brain::gateway::tests::a_turn_cancel_retires_an_inflight_failure_prep

if [ "$(uname -s)" = Darwin ]; then
    server_tests brain::gateway::tests::simulator::slow_stop_is_acknowledged_before_provider_finishes_and_next_wake_works brain::gateway::tests::simulator::stop_between_supplement_and_capture_closes_only_the_reserved_turn
fi
