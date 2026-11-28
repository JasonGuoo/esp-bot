server_tests \
    brain::task_delivery::tests:: \
    brain::tasks::tests::a_result_and_its_inbox \
    brain::tasks::tests::a_full_store \
    brain::tasks::tests::a_second_claim \
    brain::tasks::tests::summary_and_full \
    brain::tasks::tests::capacity_rejects \
    brain::tasks::tests::three_hundred_completions \
    brain::tasks::tests::an_offered_row_returns \
    brain::tasks::tests::a_summary_can_be_finished \
    brain::tasks::tests::shutdown_and_crash_recovery \
    brain::gateway::tests::a_spoken_question_on_audio \
    brain::tasks::tests::cancelling_one_task \
    brain::gateway::tests::a_question_defers \
    brain::gateway::tests::a_silent_turn \
    brain::gateway::tests::a_task_result_offer

if [ "$(uname -s)" = Darwin ]; then
    server_tests brain::gateway::tests::simulator::running_background_task_allows_two_chat_turns_and_retains_its_result
fi
