# Issue #190：Todo / Tool Loop 分层测试审计

> 归档说明：本文对应 Issue #190，是完成 #190 时形成的历史审计记录。当前行为和测试职责最终以源码、测试、`AGENTS.md` 及维护文档为准。

审计日期：2026-08-08

本清单以当前源码和测试为准，审计范围是 `qq-maid-core` 中 Tool、Respond、Pending、Storage 之间重叠的 Todo / Tool Loop 链路。目标是说明每个测试保护的独立风险，不以测试数量或代码行数为优化目标。

## 范围边界

- #186 的要求作为保留原则：跨层链路、历史兼容、真实副作用、事务和模型伪成功防护优先保留。
- #188 仍未完成，本次没有处理 ignored、skipped、无引用、失效测试资产，也没有删除 fixture 或 helper。
- 本次没有修改生产代码。
- 当前 Todo 持久化状态只有 `Pending` / `Completed`；“取消”在确认 pending 语义中表示放弃本次操作并保留 Todo，不等同于写入一个持久化的 soft-cancel 状态。

## 分层职责与测试清单

### Tool：schema、路由、选择解析与工具入口

这些测试保护模型输入如何被解析、是否提前写库、是否创建正确的 pending，以及可见编号到内部对象的绑定。Respond 的最终回复测试和 Storage 的 SQL 测试不能替代这些风险。

| 文件 | 测试 | 独立风险 |
| --- | --- | --- |
| `tests/schema.rs` | `todo_selector_schemas_allow_null_for_unused_strict_fields`; `list_todos_schema_exposes_structured_combination_filters`; `todo_selection_request_counts_only_effective_selectors`; `create_todo_schema_uses_shared_batch_limit`; `restore_todo_schema_describes_natural_language_undo_paths` | Tool schema 的 nullable 字段、组合筛选、有效 selector 判定、批量上限和自然语言恢复契约。 |
| `tests/selection.rs` | `todo_tool_scope_uses_explicit_private_and_group_context`; `todo_tool_scope_keeps_stable_private_and_group_distinct`; `prepared_number_binding_survives_previous_completion`; `same_task_query_numbers_prefer_current_list_over_stale_visible_snapshot`; `blocked_quoted_snapshot_does_not_fallback_to_last_todo_query` | owner / scope / interaction scope 的装配，以及当前查询、引用快照和旧快照之间的编号绑定优先级。 |
| `route.rs` | `explicit_todo_operations_are_high_confidence`; `weak_references_require_recent_todo_context`; `time_expressions_do_not_create_todo_status_without_todo_signal`; `todo_ascii_marker_matches_a_word_instead_of_a_substring` | 普通聊天、时间表达式和相似字符串不得误路由到 Todo flow。 |
| `visible_entity.rs` | `quoted_snapshot_account_mismatch_blocks_without_fallback`; `quoted_snapshot_group_owner_mismatch_blocks_without_fallback`; `quoted_snapshot_group_scope_mismatch_blocks_without_fallback` | 引用的可见快照不得跨账号、owner 或群 scope 使用。 |
| `tests/create.rs` | `create_tool_accepts_stable_private_scope_context`; `create_tool_places_daypart_in_time_fields_when_model_keeps_raw_content`; `create_tool_replay_with_same_call_id_does_not_duplicate_created_todo`; `create_tool_accepts_batch_at_contract_limit`; `create_tool_rejects_empty_batch_without_writes`; `create_tool_rejects_batch_over_contract_limit_without_partial_writes`; `create_tool_batch_limit_does_not_cap_existing_todo_total` | scope 写入、时间解析、调用重放幂等、批量边界和参数校验失败时不得提前写库。与 Storage 的 rollback 测试是不同风险。 |
| `tests/edit.rs` | `edit_tool_reuses_user_visible_snapshot_across_same_task_rounds`; `edit_tool_detail_patch_sets_preserves_and_clears_without_touching_other_fields`; `edit_tool_clears_visible_third_detail_and_list_no_longer_formats_it`; `create_then_edit_reference_last_updates_same_todo_without_pending`; `unresolved_last_reference_creates_todo_clarification_pending` | patch 的 preserve / set / clear 语义、快照复用、最近对象引用和无法解析时的澄清 pending。 |
| `tests/complete.rs` | `complete_tool_selection_text_discrete_deduplicates_numbers` | 离散编号解析和去重；真实完成状态由 Tool Loop / Storage 另外保护。 |
| `tests/delete.rs` | `delete_tool_number_clarification_includes_pending_candidates_without_visible_snapshot`; `delete_tool_all_completed_zero_match_does_not_create_pending`; `delete_tool_query_unique_creates_single_delete_pending`; `delete_tool_query_multiple_creates_clarification_without_snapshot_pollution`; `delete_tool_query_pending_match_creates_confirmation`; `delete_numbers_prefer_current_task_query_over_stale_visible_snapshot`; `delete_numbers_prefer_quoted_snapshot_over_latest_last_todo_query`; `delete_tool_rejects_mixed_status_bulk_selection_without_pending` | 没有快照时的澄清、零匹配不建 pending、legacy single `TodoDelete`、bulk confirmation、快照优先级、混合状态拒绝。这里明确保留 single 与 bulk 两条历史分支。 |
| `tests/merge.rs` | `merge_numbers_use_quoted_snapshot_and_physically_delete_source`; `merge_reminder_sync_failure_returns_structured_partial_failure`; `merge_source_delete_failure_replays_without_duplicate_target_update` | 目标更新、源对象物理删除、提醒同步部分失败和重放幂等。 |

### Pending：通用 envelope、Todo payload 与生命周期

通用 Pending 测试只负责 envelope / lifecycle 的结构和安全边界；Todo payload 测试负责领域字段转换；高层 pending 测试负责用户确认、真实工具执行和结果投影。三层不能合并为一层。

| 文件 | 测试 | 独立风险 |
| --- | --- | --- |
| `runtime/pending/mod.rs` | `prepared_action_round_trips_with_lifecycle_metadata`; `flat_pending_without_envelope_is_rejected`; `execution_rejects_expired_cross_user_scope_owner_and_revision`; `revised_action_invalidates_old_revision_and_supports_failed_state`; `expiry_helper_uses_explicit_timestamp` | envelope 序列化、schema、owner / scope、expiry、revision、Failed 状态和旧格式拒绝。 |
| `runtime/tools/todo/pending.rs` | `todo_payload_builds_versioned_prepared_action` | Todo action kind、bulk delete payload、scope / expiry 生成和 display snapshot 的领域转换。 |
| `storage/session/tests.rs` | `pending_execution_claim_is_atomic_and_revision_guarded`; `sqlite_reopen_restores_pending_and_last_queries`; `flat_pending_json_is_cleared_on_read`; `unknown_pending_json_is_cleared_instead_of_blocking_session`; `unsupported_prepared_action_schema_is_cleared_on_read`; `append_exchange_with_latest_merges_query_snapshot_without_overwriting_newer_fields` | SQLite session 的原子 claim、重启恢复、旧或未知 pending 清理，以及 exchange 写回时不得覆盖较新的查询快照；这些不是内存 envelope 测试。 |
| `todo/pending_tests.rs` | `inbound_classification_keeps_plain_cancel_aggregatable_without_pending`; `inbound_classification_marks_pending_input_immediate`; `prepared_bulk_delete_confirm_executes_once_and_clears_pending`; `prepared_bulk_delete_cancel_and_expiry_never_execute`; `prepared_bulk_delete_cross_scope_is_cleared_without_execution`; `inbound_classification_marks_business_commands_immediate`; `inbound_classification_marks_explicit_todo_commands_immediate`; `inbound_classification_marks_natural_todo_queries_as_normal_chat`; `invalid_single_delete_pending_scope_can_cancel_or_restart`; `todo_delete_confirm_pending_item_refreshes_snapshot_after_delete`; `todo_bulk_delete_confirm_keeps_items_whose_status_changed_after_pending_created`; `stable_group_todo_clarify_is_isolated_by_actor_interaction_session`; `todo_clarify_manage_recurring_reminder_number_resume_skips_next`; `stable_group_visible_todo_snapshots_are_isolated_by_actor`; `stable_group_standard_chat_keeps_conversation_session_without_actor_split` | pending 输入优先级、确认一次且清除、取消 / 过期 / 跨 scope 不执行、legacy single 取消与非法永久删除、删除后的快照刷新、状态变化导致的 partial skip、澄清和群 actor 隔离。 |

其中 `prepared_bulk_delete_confirm_executes_once_and_clears_pending` 仍保留：确认前 pending 存在、真实删除、用户可见成功回执、pending 清除和重复确认不重复执行。

### Respond：用户可见行为、Tool Loop 结果与伪成功防护

Respond 层不能只复述 Tool 返回结构；它必须验证真实 Tool 结果如何影响用户回复、数据库状态、pending 生命周期和最终模型失败时的降级行为。

| 文件 | 测试 | 独立风险 |
| --- | --- | --- |
| `runtime/tools/todo/success_guard/tests.rs` | `non_success_chat_passes_without_tool_result`; `implicit_todo_success_candidates_require_time_and_task_behavior`; `explicit_and_implicit_candidates_use_distinct_success_claim_scopes`; `explicit_non_success_explanation_passes_without_tool_result`; `capability_and_status_explanations_pass_without_tool_result`; `todo_success_reply_without_tool_result_is_blocked`; `todo_success_reply_requires_successful_structured_result`; `tool_failure_reply_prefers_business_error_over_dependency_skip` | 纯函数层的文本分类、成功声明范围、无 Tool Result 阻断、结构化结果验证和业务错误优先级。它只覆盖分类组合，不覆盖用户可见集成副作用。 |
| `respond/tests/todo_agent/guard.rs` | `todo_create_intent_without_tool_call_does_not_leak_fake_success_reply`; `implicit_todo_create_claim_without_tool_call_is_blocked`; `implicit_buy_groceries_claim_without_tool_call_is_blocked`; `implicit_task_execution_reply_without_create_claim_is_allowed`; `implicit_task_create_claim_is_still_blocked`; `todo_detail_clear_promise_without_tool_call_is_blocked`; `todo_fake_success_with_followup_instruction_is_still_blocked`; `todo_mixed_unsupported_and_fake_success_reply_is_still_blocked`; `todo_capability_question_without_tool_call_is_not_required_tool_blocked`; `todo_unsupported_operation_reply_without_tool_call_is_not_blocked`; `todo_missing_argument_reply_without_tool_call_is_not_blocked`; `todo_edit_guard_requires_successful_update_result`; `todo_write_result_is_returned_when_final_agent_round_fails`; `todo_edit_tool_false_result_does_not_pass_success_guard`; `todo_delete_pending_item_false_deleted_text_does_not_pass_success_guard`; `todo_delete_completed_tool_failure_cannot_be_reported_as_success`; `non_todo_chat_phrase_does_not_mutate_when_model_calls_no_tool`; `last_reference_complete_without_tool_blocks_fake_success_reply` | 模型没有真实 Tool 调用、Tool 失败、`ok=false` 或 final agent round 失败时，不得伪造成功；同时保留真实写入结果、pending 和原数据库状态。纯 success guard 单测不能替代这些用户可见集成测试。 |
| `respond/tests/agent_turn/todo.rs` | `todo_tool_ok_false_without_error_code_is_failed_outcome`; `todo_clarification_is_not_marked_as_write_success`; `todo_business_failure_keeps_root_error_before_dependency_skip`; `todo_success_then_failure_is_partial_success_and_keeps_database_change`; `multiple_successful_todo_writes_share_one_background_snapshot`; `only_list_todos_success_does_not_claim_todo_write_success` | Tool outcome 的失败分类、澄清、dependency skip 优先级、部分成功和“只读 list 不等于写入成功”。 |
| `respond/tests/todo_tool_loop.rs` | `private_tool_loop_registers_todo_tools_and_keeps_internal_ids_hidden`; `quoted_todo_reminder_completion_uses_request_whitelist_and_survives_final_failure`; `quoted_completed_todo_restore_remains_exposed_and_uses_same_scope`; `unfinished_question_excludes_restore_tool_from_request_registry`; `natural_language_undo_restores_most_recently_completed_todo`; `group_tool_loop_todo_visible_snapshot_uses_actor_interaction_session`; `explicit_pending_query_then_tool_loop_complete_first_uses_latest_snapshot`; `explicit_date_query_then_tool_loop_complete_first_uses_date_snapshot`; `explicit_todo_query_then_tool_loop_complete_first_uses_latest_snapshot`; `explicit_completed_query_then_tool_loop_restore_first_uses_latest_snapshot`; `explicit_empty_query_clears_old_snapshot_before_number_mutation`; `explicit_query_then_status_changes_returns_precise_missing_error` | Tool 白名单、内部 ID 隐藏、quoted snapshot、完成 / restore 真实状态变化、final failure fallback、最近动作、actor 隔离、查询后编号绑定、旧快照清理和状态变化后的精确 missing 错误。 |
| `respond/tests/todo_deterministic.rs` | `deterministic_complete_uses_snapshot_real_id_without_llm_request`; `deterministic_restore_uses_snapshot_real_id_without_llm_request`; `repeated_same_message_deterministic_complete_reuses_dedup_output`; `ambiguous_or_missing_snapshot_keeps_tool_loop`; `out_of_range_number_keeps_tool_loop`; `delete_never_short_circuits_because_it_needs_confirmation`; `chinese_ordinal_and_mixed_actions_keep_tool_loop`; `channel_and_unknown_conversation_kinds_keep_tool_loop_even_with_valid_snapshot` | deterministic shortcut 的边界、真实 ID、dedup、歧义 / 越界回退、删除必须确认及不同 conversation kind 的安全边界。 |
| `respond/tests/todo/clarification.rs` | `todo_clarification_llm_tool_call_completes_candidate_scope`; `todo_clarification_control_ask_again_keeps_pending_without_mutation`; `todo_clarification_cancel_and_expiry_do_not_mutate`; `todo_clarification_number_target_changed_keeps_pending_without_side_effect`; `todo_clarification_candidate_scope_does_not_persist_as_last_query`; `todo_clarification_loop_error_marks_failed_and_blocks_repeat_execution`; `todo_clarification_no_tool_reply_updates_question_and_keeps_pending`; `todo_clarification_delete_tool_replaces_with_confirmation_pending`; `todo_clarification_out_of_range_number_keeps_pending_without_side_effect`; `todo_clarification_control_abandon_clears_pending_without_mutation` | 用户输入到澄清恢复的完整链路、控制词、候选范围、无副作用、失败重试阻断、删除 confirmation 替换和 pending 生命周期。 |
| `respond/tests/todo_agent/pending.rs` | `todo_delete_completed_item_accepts_delete_tool_pending_result`; `todo_delete_completed_pending_confirmation_is_verified_by_real_tool_result` | Respond 投影 legacy single pending，以及确认文案必须由真实 Tool 结果验证，不能被模型成功文案替代。 |
| `respond/tests/todo_agent/write.rs` | `todo_create_receipt_shows_full_user_visible_card`; `todo_edit_receipt_shows_final_detail_card`; `todo_edit_receipt_clears_detail_after_successful_tool_result`; `todo_tool_loop_clears_third_and_fourth_details`; `todo_complete_receipt_reuses_full_user_visible_card`; `todo_edit_second_item_uses_latest_visible_snapshot`; `todo_internal_list_before_write_is_not_user_visible_query`; `todo_write_with_explicit_list_does_not_append_auto_related_list` | 用户可见卡片、详情清除、编号快照、内部 list 不外泄以及不自动追加未请求列表。 |
| `respond/tests/todo_receipt.rs` | `todo_complete_receipt_is_lightweight_and_refreshes_pending_snapshot`; `todo_complete_receipt_refreshes_pending_snapshot_at_ten_item_limit` | 完成后的轻量回执和十条上限下的剩余快照刷新。 |
| `respond/tests/todo/query.rs` | `todo_query_writes_visible_snapshot_for_tool_followup`; `todo_list_command_filters_recurring_type`; `todo_list_command_combines_time_status_and_fuzzy_keyword`; `todo_list_command_rejects_conflicting_time_filters_with_help`; `todo_list_command_supports_completed_no_due_and_keyword_filters`; `todo_list_duplicate_status_is_deduplicated_in_condition`; `todo_list_overdue_pending_is_order_independent`; `todo_list_status_conflicts_are_order_independent`; `todo_pending_list_shows_ten_and_reports_truncation_total`; `natural_todo_queries_no_longer_hit_deterministic_shortcuts` | 用户看到的筛选、编号快照、冲突提示、十条截断和真实 total；不能被 Storage 的 SQL 条件测试替代。 |
| `respond/tests/todo_agent/query.rs` | `ordinary_chat_response_does_not_inherit_old_todo_visible_snapshot`; `natural_language_tool_query_combines_tomorrow_status_and_keyword`; `natural_language_tool_query_supports_fuzzy_keyword_search`; `list_todos_due_date_receipt_preserves_filtered_visible_snapshot`; `list_todos_completed_date_range_receipt_uses_completed_at_snapshot`; `explicit_todo_command_aliases_and_filters_stay_deterministic`; `natural_language_todo_queries_enter_tool_loop_instead_of_shortcut`; `todo_completed_lists_show_up_to_ten_without_old_five_item_collapse`; `todo_date_filter_shows_nine_without_old_five_item_collapse`; `todo_all_caps_at_ten_and_reports_real_total`; `explicit_todo_and_full_result_restore_use_visible_snapshot`; `full_result_replays_all_structured_todo_filter_combinations`; `todo_retry_keeps_replay_context_on_final_truncated_list_result` | Agent 查询的用户可见筛选、旧快照隔离、十条限制与真实 total、完整结果 replay、重试上下文和后续 restore 的对象绑定。 |
| `respond/tests/todo/rendering.rs` | `todo_single_status_lists_render_board_style_and_remember_visible_order`; `todo_single_status_lists_render_empty_notices`; `todo_pending_list_shows_reminder_without_due_time`; `explicit_status_queries_remember_visible_order`; `todo_all_renders_grouped_board_and_remembers_visible_order`; `completed_time_query_still_updates_visible_snapshot` | 用户可见列表样式、空结果、提醒字段、分组和 completed time 的快照。 |
| `respond/tests/todo/commands.rs` | `todo_root_aliases_list_pending_items`; `todo_daily_reminder_command_updates_private_preference`; `todo_daily_reminder_command_does_not_enable_group_push`; `group_todo_defaults_to_actor_personal_owner` | slash alias、偏好设置和群 Todo owner 的命令路由行为。 |
| `respond/tests/todo/group_admin.rs` | `group_owner_and_admin_can_list_all_current_group_creators`; `ordinary_member_and_private_chat_cannot_use_group_todo_management`; `group_list_uses_full_platform_account_and_group_scope`; `group_admin_delete_cancels_claimed_reminder_before_push`; `group_admin_delete_without_reminder_uses_conditional_message`; `group_delete_snapshot_cannot_cross_actor_group_or_role_change` | 群角色、真实平台账号、群 scope、提醒取消和管理员删除的用户可见分支；与 Storage 的事务测试互补。 |
| `respond/tests/slash_pending.rs` | `registered_todo_command_keeps_existing_pending_priority`; `unknown_slash_does_not_consume_todo_delete_confirmation`; `unknown_slash_does_not_resume_todo_clarification_tool_loop` | slash 命令不得错误消费或绕过 pending。 |
| `respond/tests/agent_turn/weather.rs` | `weather_success_and_todo_success_are_both_rendered_in_order`; `readonly_weather_result_preserves_model_advice`; `weather_only_outcome_does_not_bypass_implicit_todo_success_verification`; `weather_only_outcome_preserves_non_todo_analysis_reply`; `weather_and_real_todo_create_keep_both_trusted_results`; `conditional_weather_and_todo_request_uses_tool_loop`; `weather_success_and_todo_failure_keep_fact_and_error`; `weather_failure_and_todo_success_keep_error_and_side_effect`; `weather_failure_and_dependency_skipped_todo_keep_root_cause`; `only_weather_tool_renders_fact_card` | 跨 Tool Loop 结果的顺序、非 Todo 结果不得伪造 Todo 成功、真实 Todo 副作用保留，以及业务错误不能被依赖 skip 覆盖。 |

### Storage：持久化、授权过滤、排序与事务

Storage 测试保护 SQL / SQLite 和事务边界。上层测试验证用户行为，不能替代这些底层数据安全和 rollback 证明。

| 文件 | 测试 | 独立风险 |
| --- | --- | --- |
| `todo/storage/tests.rs` | `store_isolates_owners_and_deletes`; `sqlite_ids_are_stable_and_not_reused_after_delete`; `create_many_rolls_back_when_later_draft_is_invalid`; `complete_many_with_recurrence_rolls_back_when_later_advance_fails`; `sqlite_store_persists_after_reopen_without_json_todo_dir`; `delete_completed_by_ids_filters_owner_scope_and_status_in_transaction` | owner 隔离、删除后 ID 不复用、批量 create / recurrence completion 原子 rollback、SQLite reopen 和 owner / scope / status 条件删除。 |
| `todo/storage/tests.rs` | `pending_list_sorts_by_due_time_then_id_without_changing_all_view`; `list_by_due_date_matches_date_and_datetime_but_excludes_no_time`; `completed_at_filter_uses_shanghai_date`; `shared_query_defaults_to_ten_and_reports_total_count`; `shared_query_rejects_non_pending_overdue_statuses`; `shared_query_combines_time_status_keyword_and_keeps_scope_isolation` | 稳定排序、日期字段语义、上海时区、分页 total、overdue 状态约束和组合筛选隔离。 |
| `todo/storage/tests.rs` | `infers_common_chinese_dates`; `enrich_draft_time_from_text_uses_daypart_default_datetime`; `enrich_draft_time_from_text_preserves_date_and_daypart_combo`; `enrich_draft_time_from_text_does_not_override_explicit_datetime`; `enrich_draft_time_from_text_sets_relative_minute_reminder_only`; `explicit_due_at_is_not_overwritten_by_reminder`; `reminder_only_create_keeps_due_at_empty`; `create_without_time_keeps_due_fields_empty` | Tool 时间输入落库前的日期、时段、相对提醒、显式值优先级和空 due 字段语义。 |
| `todo/storage/tests.rs` | `private_reminder_owner_query_collapses_same_target_scopes_and_filters_non_private_pending`; `private_reminder_owner_query_reports_conflicts_and_invalid_scopes` | owner 与真实私聊投递目标的对应关系，以及冲突 / 非法 scope。 |
| `todo/storage/group_admin_tests.rs` | `group_admin_query_crosses_creators_but_not_group_or_private_scope`; `group_admin_delete_rechecks_exact_group_scope_and_pending_status`; `notification_cancel_failure_rolls_back_group_todo_delete` | 群管理员跨创建者查询、精确群 scope / pending 重校验和通知取消失败 rollback。 |

## 重叠链路的保留判断

| 链路 | 高层保护 | 底层保护 | 判断 |
| --- | --- | --- | --- |
| 批量创建 / 完成 | Tool 参数上限、空输入和真实用户调用是否提前写入 | Storage 中途失败的 transaction rollback | 两者失败点不同，均保留。 |
| 删除确认 | Respond 的确认文案、真实删除结果和 pending 生命周期 | Storage 的 owner / scope / status SQL 过滤 | 用户行为和 SQL 授权不同，均保留。 |
| legacy single / bulk | `TodoDelete` 与 `TodoBulkDelete` 的 pending 创建、确认和结果投影 | payload / envelope 结构 | 历史兼容和不同语义，均保留。 |
| 取消 / 永久删除 | 取消或过期不改变 Todo；确认后按状态永久删除 | 条件删除和事务 | “不执行”与“真实物理删除”是相反风险，均保留。 |
| partial skip | 状态变化后用户回执、剩余对象和 pending 清除 | SQL 返回 `deleted_count` / `skipped_ids` | 底层过滤不能证明用户可见回执，均保留。 |
| 模型伪成功 | Respond 端没有 Tool、Tool 失败或 final failure 时的用户回复和 DB 状态 | success guard 的文本 / structured result 分类 | 纯分类与真实链路不同，均保留。 |
| 编号 / 快照 | 用户可见编号、quoted snapshot、actor scope 和后续操作 | selector / SQL 查询和排序 | 可见上下文绑定与数据查询不同，均保留。 |
| 澄清 / pending | 用户输入、候选恢复、控制词、失败和清理 | envelope revision / expiry / atomic claim | 状态机和持久化安全不同，均保留。 |

## 本次唯一缩减

`runtime/tools/todo/pending_tests.rs::prepared_bulk_delete_confirm_executes_once_and_clears_pending` 原先在确认前重复断言：

- `scope_key == "group:g1"`
- `expires_at` 非空
- `revision == 1`

这些是通用 envelope / Todo payload 的内部字段，已有以下底层证据：

- `runtime/pending/mod.rs::prepared_action_round_trips_with_lifecycle_metadata` 覆盖 scope、expiry、revision 的 envelope 生命周期；
- `runtime/tools/todo/pending.rs::todo_payload_builds_versioned_prepared_action` 覆盖 Todo payload 到 prepared action 的领域转换。

因此本层只改为断言确认前 `pending_operation.is_some()`，并保留真实删除、成功回执、pending 清除和重复确认不重复执行。没有删除任何跨层行为断言，也没有缩减 legacy single、取消 / 过期、partial skip、事务 rollback 或伪成功测试。

## 验证

- `cargo fmt --all -- --check`
- `cargo test -p qq-maid-core --lib prepared_bulk_delete_confirm_executes_once_and_clears_pending`
- `cargo test -p qq-maid-core --lib prepared_action_round_trips_with_lifecycle_metadata`
- `cargo test -p qq-maid-core --lib todo_payload_builds_versioned_prepared_action`
- `cargo test -p qq-maid-core --lib pending`：80 passed
- `cargo test -p qq-maid-core --lib todo`：337 passed
- `git diff --check`

本次未运行全 workspace CI、release 构建或真实运行环境验证，因为修改仅涉及一个测试断言和审计文档，未涉及生产代码、启动、配置、依赖或发布路径。
