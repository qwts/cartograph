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
| T-0001 | AC-0001 | reserved | — | M-next: GitHub ingest (US-0001) |
| T-0002 | AC-0002 | reserved | — | topology manifest (US-0001) |
| T-0003 | AC-0003 | reserved | — | auth failure path (US-0001) |
| T-0004 | AC-0004 | rust | adapters-lang-ts::extracts_express_endpoints_not_arbitrary_calls, adapters-lang-ts::endpoint_receiver_must_come_from_framework_factory, adapters-lang-ts::handles_edges_bind_named_and_anonymous_handlers | Express registry; receiver proven from factory |
| T-0005 | AC-0005 | rust | adapters-lang-ts::call_edges_are_symbol_to_symbol, adapters-lang-ts::imports_resolve_relative_files_and_modules | intra-proc + import-bound; typed inter-proc still open (#2) |
| T-0006 | AC-0006 | rust | adapters-lang-ts::every_fact_carries_confirmed_t0_provenance, adapters-lang-ts::evidence_spans_point_at_the_actual_source | plus story:Atlas/EvidencePanel/WithSource and manual:MT-M1-01 |
| T-0007 | AC-0007 | rust | iac::resources_data_and_modules_become_resource_nodes, iac::interpolation_references_build_the_dag, iac::depends_on_is_distinct_from_references | resource DAG from HCL |
| T-0008 | AC-0008 | rust | iac::capability_registry_emits_triggers_deterministically, iac::capability_registry_emits_subscribes, iac::iam_policy_grants_reference_target_resources_with_actions | TRIGGERS/SUBSCRIBES/GRANTS via registry, plus manual:MT-M2-01 |
| T-0009 | AC-0009 | reserved | — | M6: terraform state/plan T1 enrichment |
| T-0010 | AC-0010 | rust | events::literal_channel_ids_link_producer_and_consumer, events::kafka_producer_and_consumer_stitch_across_files, adapters-lang-ts::event_receiver_must_come_from_sdk_constructor | literal ids stitch; receivers proven from SDK |
| T-0011 | AC-0011 | rust | events::config_resolved_channel_is_confirmed_via_env_file, events::config_index_parses_env_files_deterministically, adapters-lang-ts::bracketed_env_access_is_an_env_ref | env-file resolver; JSON/YAML config at M5 with the manifest |
| T-0012 | AC-0012 | rust | events::computed_channel_emits_gap_with_reason, events::missing_env_key_emits_gap_naming_the_key, adapters-lang-ts::let_bound_identity_stays_computed | T0→Gap with reason; T1/T2 rungs join at M6/M7, cross-repo at M5 |
| T-0013 | AC-0013 | reserved | — | M4: Screen/Component + FETCHES |
| T-0014 | AC-0014 | reserved | — | M4: fetch-site resolution |
| T-0015 | AC-0015 | rust | flowtracer::hops_record_tier_across_the_full_chain, flowtracer::orphan_channel_is_a_trigger_published_channel_is_not | all hops T0 at M3; T1–T3 rungs join M6–M8 |
| T-0016 | AC-0016 | rust | flowtracer::gap_truncates_the_branch, flowtracer::depth_bound_marks_the_flow_partial_not_verified, spec::flow_dossier_renders_sequence_and_provenance_table | truncation in trace + visibly broken in the artifact |
| T-0017 | AC-0017 | rust | flowtracer::flow_status_follows_the_scoring_rule | Verified/Partial/Inferred per §5.3, plus manual:MT-M3-01 |
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
