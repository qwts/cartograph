# Test-trace map

Binds every reserved test id (`T-XXXX`, from [`US-TM.md`](US-TM.md)) to its
realization. `kind` is one of:

- `rust` — `crate::test_function` (existence CI-verified)
- `story` — Storybook story with a `play` interaction test (CI-verified)
- `manual` — procedure in [`manual-tests.md`](manual-tests.md) (CI-verified)
- `reserved` — not yet realized; the note says which milestone realizes it

`check-traceability.mjs` fails CI when a US-TM test id is missing here or an
automated reference names a test that does not exist. New ACs land with their
rows in the same PR (see AGENTS.md).

| T id | AC | Kind | Reference | Note |
|------|----|------|-----------|------|
| T-0001 | AC-0001 | rust | ingest::clone_lists_repo_with_commit_sha, ingest::repo_urls_parse_to_identities, app::identical_repos_do_not_collide_in_one_graph | offline file:// fixtures; SHA listed in UI, plus story:Shell/IngestCard/WithClonedRepo, live clone manual at the milestone boundary |
| T-0002 | AC-0002 | rust | ingest::manifest_parses_repos_layers_and_env, ingest::manifest_rejects_unknown_layers, app::manifest_local_paths_beat_owner_name_shorthand, events::manifest_identities_override_env_files, app::system_manifest_applies_hints_and_identities_at_ingest | layer hints + identities applied at ingest; UI listing, plus story:Shell/IngestCard/WithSystemManifest |
| T-0003 | AC-0003 | rust | ingest::failed_clone_leaves_nothing_behind, ingest::auth_errors_carry_remediation | typed remediation; live 401 path manual at the milestone boundary |
| T-0004 | AC-0004 | rust | adapters-lang-ts::extracts_express_endpoints_not_arbitrary_calls, adapters-lang-ts::endpoint_receiver_must_come_from_framework_factory, adapters-lang-ts::handles_edges_bind_named_and_anonymous_handlers | Express registry; receiver proven from factory |
| T-0005 | AC-0005 | rust | adapters-lang-ts::call_edges_are_symbol_to_symbol, adapters-lang-ts::imports_resolve_relative_files_and_modules | intra-proc + import-bound; typed inter-proc still open (#2) |
| T-0006 | AC-0006 | rust | adapters-lang-ts::every_fact_carries_confirmed_t0_provenance, adapters-lang-ts::evidence_spans_point_at_the_actual_source | plus story:Atlas/EvidencePanel/WithSource and manual:MT-M1-01 |
| T-0007 | AC-0007 | rust | iac::resources_data_and_modules_become_resource_nodes, iac::interpolation_references_build_the_dag, iac::depends_on_is_distinct_from_references | resource DAG from HCL |
| T-0008 | AC-0008 | rust | iac::capability_registry_emits_triggers_deterministically, iac::capability_registry_emits_subscribes, iac::capability_registry_routes_nested_lb_listener_action, iac::iam_policy_grants_reference_target_resources_with_actions | TRIGGERS/ROUTES/SUBSCRIBES/GRANTS via registry, plus manual:MT-M2-01 |
| T-0009 | AC-0009 | rust | dynamic::state_and_plan_shapes_both_parse, dynamic::sensitive_values_are_redacted_never_stored, dynamic::observed_values_enrich_t0_resources_with_dynamic_provenance, dynamic::observation_supersedes_placeholder_refs, dynamic::backing_candidates_name_the_channel_from_observed_identity, dynamic::redacted_identity_never_becomes_a_channel, spec::backs_edge_renders_the_channel_as_a_cylinder, app::observed_state_backs_channels_and_resolves_placeholders, app::state_json_resolves_from_manifest_directory_for_both_input_forms | terraform show -json T1 enrichment with the BACKS join at M6, plus manual:MT-M6-01 |
| T-0010 | AC-0010 | rust | events::literal_channel_ids_link_producer_and_consumer, events::kafka_producer_and_consumer_stitch_across_files, adapters-lang-ts::event_receiver_must_come_from_sdk_constructor, app::cross_repo_flow_stitches_via_literal_channel_identity | literal ids stitch within and across repos (M5), plus manual:MT-M5-01 |
| T-0011 | AC-0011 | rust | events::config_resolved_channel_is_confirmed_via_env_file, events::config_index_parses_env_files_deterministically, adapters-lang-ts::bracketed_env_access_is_an_env_ref | env-file resolver; JSON/YAML config at M5 with the manifest |
| T-0012 | AC-0012 | rust | events::computed_channel_emits_gap_with_reason, events::missing_env_key_emits_gap_naming_the_key, adapters-lang-ts::let_bound_identity_stays_computed, dynamic::otlp_jsonl_parses_messaging_and_http_span_attributes, dynamic::otel_observation_resolves_channel_gap_and_enriches_http_endpoint, dynamic::ambiguous_trace_identity_keeps_explicit_gaps, app::otel_trace_resolves_runtime_channel_gap_with_observed_provenance | T0→explicit Gap; OTLP/JSONL T1 fills only uniquely matched slots at M6; T2 joins at M7, plus manual:MT-M6-02 |
| T-0013 | AC-0013 | rust | adapters-lang-ts::react_router_routes_become_screens_with_renders, adapters-lang-ts::next_pages_convention_yields_screens, adapters-lang-ts::jsx_usage_becomes_renders_edges | Route import-proven; components capitalized-in-tsx |
| T-0014 | AC-0014 | rust | events::resolvable_fetch_urls_confirm_against_endpoints, events::unresolvable_fetches_emit_gaps_with_reasons, adapters-lang-ts::fetch_and_axios_sites_are_detected_and_classified, adapters-lang-ts::shadowed_fetch_is_not_a_fetch_site, adapters-lang-ts::nested_method_key_does_not_set_the_http_method | exact + :param template match; Gap on computed/no-match/ambiguous |
| T-0015 | AC-0015 | rust | flowtracer::hops_record_tier_across_the_full_chain, flowtracer::orphan_channel_is_a_trigger_published_channel_is_not, flowtracer::screen_anchored_flow_walks_the_full_chain, flowtracer::unfetched_endpoints_remain_triggers, flowtracer::endpoint_fetched_only_by_an_unrendered_component_keeps_its_flow, adapters-lang-ts::fetch_in_nested_component_anchors_at_the_nearest_component | Screen anchor at M4 (T1–T3 rungs join M6–M8), plus manual:MT-M4-01 |
| T-0016 | AC-0016 | rust | flowtracer::gap_truncates_the_branch, flowtracer::depth_bound_marks_the_flow_partial_not_verified, flowtracer::unresolved_fetch_truncates_the_screen_flow, spec::flow_dossier_renders_sequence_and_provenance_table | truncation in trace + visibly broken in the artifact |
| T-0017 | AC-0017 | rust | flowtracer::flow_status_follows_the_scoring_rule | Verified/Partial/Inferred per §5.3; status+score chips in UI, plus story:Atlas/FlowsCard/Populated and manual:MT-M3-01 |
| T-0018 | AC-0018 | rust | core-prov::provenance_serde_round_trips, core-prov::content_hash_is_deterministic_and_content_sensitive, core-prov::confidence_ceilings_match_spec | provenance shape + hashing |
| T-0019 | AC-0019 | rust | core-prov::r_int_1_t2_t3_never_touch_confirmed, core-prov::provenance_rejects_confidence_above_ceiling | R-INT-1 as executable code |
| T-0020 | AC-0020 | reserved | — | M8: propose-only agent enforcement |
| T-0021 | AC-0021 | reserved | — | M7: T2 proposals |
| T-0022 | AC-0022 | reserved | — | M7: eval precision floor |
| T-0023 | AC-0023 | reserved | — | M8: egress fail-closed |
| T-0024 | AC-0024 | reserved | — | M8: consent payload dialog |
| T-0025 | AC-0025 | reserved | — | M8: decision persistence |
| T-0026 | AC-0026 | reserved | — | M9: Atlas layer filter |
| T-0027 | AC-0027 | reserved | — | M9: confidence overlay |
| T-0028 | AC-0028 | story | Atlas/EvidencePanel/WithSource, Atlas/EvidencePanel/WindowedLargeFile | evidence panel groundwork; full Atlas node-select at M9 (plus manual:MT-M1-01) |
| T-0029 | AC-0029 | reserved | — | M9: Flow Inspector sequence |
| T-0030 | AC-0030 | reserved | — | M9: Gap cards |
| T-0031 | AC-0031 | reserved | — | M9: verified-only toggle |
| T-0032 | AC-0032 | reserved | — | M9: inline provenance in Workbench |
| T-0033 | AC-0033 | reserved | — | M9: curation persistence |
| T-0034 | AC-0034 | reserved | — | M10: export honors R-INT-5 |
| T-0035 | AC-0035 | reserved | — | M9–M10: full artifact set |
| T-0036 | AC-0036 | reserved | — | M9: found-ADR linking |
| T-0037 | AC-0037 | reserved | — | M9: recovered ADRs marked inferred |
| T-0038 | AC-0038 | reserved | — | M9: drift register |
| T-0039 | AC-0039 | rust | adapters-lang-ts::dir_walk_skips_noise_and_is_deterministic, iac::dir_walk_is_deterministic_and_skips_dot_terraform, spec::output_is_deterministic | extractor-level determinism; full re-ingest hash invariant lands at M10 |
| T-0040 | AC-0040 | reserved | — | M10: delta re-ingest |
| T-0041 | AC-0041 | reserved | — | M9: unauth endpoint findings |
| T-0042 | AC-0042 | reserved | — | M9: over-broad GRANTS findings |
| T-0043 | AC-0043 | rust | iac::capability_registry_routes_api_gateway_integrations | API Gateway v1 REST API to direct integration target via ROUTES |
| T-0044 | AC-0044 | rust | iac::capability_registry_triggers_through_lambda_permissions | source ARN to Lambda function via TRIGGERS |
| T-0045 | AC-0045 | rust | iac::capability_registry_triggers_lambda_at_edge_from_nested_cache_behaviors | default/ordered cache behavior traversal with distribution as source |
| T-0046 | AC-0046 | rust | iac::capability_registry_triggers_aws_pipes | EventBridge Pipes source to target via TRIGGERS |
| T-0047 | AC-0047 | rust | iac::iam_policy_grants_chase_same_extraction_policy_document, iac::iam_policy_document_chase_spans_files_in_directory, iac::missing_or_unresolved_policy_document_keeps_explicit_grant | same-source and cross-file joins; fail-closed fallback with document actions/evidence |
| T-0048 | AC-0048 | rust | iac::local_modules_expand_under_scoped_addresses_and_edges, iac::nested_local_modules_stop_at_cycles_deterministically, iac::remote_outside_and_symlinked_modules_remain_leaf_nodes | EventBridge-style ../../ source; scoped internal facts; confinement and cycle guard |
