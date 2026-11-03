def pct($a;$b): if $b==0 then null else 100*$a/$b end;
{
 schema:"slopninja-assignment-gameability-summary-v1",
 service:(.service|group_by(.cartel_uids)|map(. as $rows|
   (map(.cartel_requester_slots)|add) as $n|
   {cartel_uids:.[0].cartel_uids,cartel_requester_slots:$n,
    mandatory_first_percent:pct((map(.mandatory_first.favorable)|add);$n),
    menu_percent:pct((map(.choose_any_three.favorable)|add);$n),
    capped_skip_delivered_percent:pct((map(.requester_abandonment_capped.favorable)|add);(map(.requester_abandonment_capped.delivered)|add)),
    aggregate_only_interface_violations:(map(.requester_abandonment_aggregate_only.uids_failing_at_least_one_interface)|add),
    cartel_uid_epochs:(length*.[0].cartel_uids),
    provider_failures:(map(.provider_selective_failures.count)|add),
    recovered_tickets:(map(.provider_selective_failures.tickets_recovered_by_fallback)|add),
    exhausted_tickets:(map(.provider_selective_failures.exhausted_tickets)|add),
    failures_left_if_incorrectly_erased:(map(.provider_selective_failures.failures_if_incorrectly_erased_after_success)|add),
    correct_gate_passes_when_all_unfavorable_slots_abandoned:(map(.requester_abandonment_all_unfavorable.cartel_requesters_passing_correct_gate)|add),
    incorrect_delivered_only_gate_passes:(map(.requester_abandonment_all_unfavorable.incorrect_delivered_only_gate_would_pass)|add)})),
 panels:(.panels|group_by([.cartel_uids,.strongest_cartel])|map(
    (map(.trials)|add) as $n|
    {cartel_uids:.[0].cartel_uids,strongest_cartel:.[0].strongest_cartel,trials:$n,
    panel_majority_percent:pct((map(.median_panel_cartel_majority)|add);$n),panel_theory_percent:(100*.[0].median_panel_majority_theory),
    joint_panel_and_cartel_batch_percent:pct((map(.majority_with_other_cartel_uid_in_b_batch)|add);$n),joint_theory_percent:(100*.[0].joint_theory),
    reviewer_majority_percent:pct((map(.reviewer_cartel_majority)|add);$n),reviewer_theory_percent:(100*.[0].reviewer_majority_theory),
    cartel_submitters:(map(.modeled_cartel_submitters)|add),targeted_bad_submissions:(map(.targeted_bad_submissions_on_known_captured_reviewers)|add)})),
 panel_size_sensitivity_20_percent:[.size_sensitivity[]|select(.cartel_uids==20)],
 accounting:.accounting,
 checks:{provider_and_requester_gates:all(.service[];.provider_selective_failures.all_providers_pass_per_interface_and_aggregate and .requester_abandonment_capped.all_interfaces_and_aggregate_pass),
   panel_batch_exclusion:all(.panels[];.exact_panel_uid_exclusion_always_holds),
   reviewer_exclusions:all(.panels[];.reviewer_uids_disjoint_from_all_involved_a_b_uids),
   all_reviews_present:all(.panels[];.all_modeled_submissions_receive_three_reviews)}
}
