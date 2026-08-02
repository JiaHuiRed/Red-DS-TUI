//! Engine event polling for the TUI event loop.
//!
//! Extracted from `run_event_loop` (crates/tui/src/tui/ui.rs) so the
//! ~950-line engine event dispatch lives in one self-contained module.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;

use crate::audit::log_sensitive_event;
use crate::client::DeepSeekClient;
use crate::config::Config;
use crate::core::engine::EngineHandle;
use crate::core::events::Event as EngineEvent;
use crate::core::ops::Op;
use crate::models::{ContentBlock, Message};
use crate::session_manager::SessionManager;
use crate::task_manager::SharedTaskManager;
use crate::tools::subagent::SubAgentStatus;
use crate::tui::event_broker::EventBroker;
use crate::tui::footer_ui::{friendly_subagent_progress, is_noisy_subagent_progress};
use crate::tui::notifications;
use crate::tui::persistence_actor::{self, PersistRequest};
use crate::tui::plan_prompt::PlanPromptView;
use crate::tui::streaming_thinking;
use crate::tui::subagent_routing::{
    handle_subagent_mailbox, reconcile_subagent_activity_state, sort_subagents_in_place,
};
use crate::tui::tool_routing::{
    handle_tool_call_complete, handle_tool_call_started, maybe_add_patch_preview,
};
use crate::tui::user_input::UserInputView;
use crate::tui::views::subagent_view_agents;

use crate::tui::app::{App, AppMode, QueuedMessage, TurnCacheRecord};
use crate::tui::approval::{
    ApprovalMode, ApprovalRequest, ApprovalView, ElevationRequest, ElevationView,
};
use crate::tui::history::{HistoryCell, summarize_tool_output};
use crate::tui::views::ModalKind;
use super::{
    AppTerminal, PERIODIC_FULL_REPAINT_EVERY_N, TranslationEvent, append_streaming_text,
    apply_engine_error_to_app, build_session_snapshot, derive_session_title,
    ensure_streaming_assistant_history_cell, extract_reasoning_header,
    ignore_stale_stream_event_while_idle, is_session_approved_for_tool, is_session_denied_for_key,
    merge_pending_steers, pause_terminal, plan_next_step_prompt, push_assistant_message,
    refresh_active_task_panel, resume_terminal, sanitize_stream_chunk,
    suppress_engine_event_after_local_cancel,
};

/// Result of one engine-event poll pass, consumed by the main loop.
pub(super) struct EngineEventPollResult {
    pub(super) received_engine_event: bool,
    pub(super) transcript_batch_updated: bool,
    pub(super) queued_to_send: Option<QueuedMessage>,
}

/// Drain the engine's event channel non-blockingly and apply every event
/// to `app` state. Returns flags the main loop needs for its own bookkeeping
/// (history refresh, queue dispatch, repaint).
#[allow(clippy::too_many_arguments)]
pub(super) async fn poll_engine_events(
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &Config,
    engine_handle: &EngineHandle,
    task_manager: &SharedTaskManager,
    event_broker: &EventBroker,
    translation_client: &Option<Arc<DeepSeekClient>>,
    translation_tx: &UnboundedSender<TranslationEvent>,
    current_streaming_text: &mut String,
    pending_translations: &mut usize,
    pending_thinking_translations: &mut usize,
    last_task_refresh: &mut Instant,
    last_status_frame: &mut Instant,
    force_terminal_repaint: &mut bool,
    draws_since_last_full_repaint: u64,
    terminal_paused_at: &mut Option<Instant>,
) -> Result<EngineEventPollResult> {
    let mut received_engine_event = false;
    let mut transcript_batch_updated = false;
    let mut queued_to_send: Option<QueuedMessage> = None;
    {
        let mut rx = engine_handle.rx_event.write().await;
        while let Ok(event) = rx.try_recv() {
            received_engine_event = true;
            if app.suppress_stream_events_until_turn_complete {
                if matches!(event, EngineEvent::TurnStarted { .. }) {
                    // Ctrl+C can race with the engine's per-turn token
                    // reset: the first cancel may hit the previous token
                    // if SendMessage is queued but TurnStarted has not
                    // arrived yet. Reassert cancellation once the real
                    // turn starts, then keep hiding its queued deltas.
                    engine_handle.cancel();
                    continue;
                }
                if suppress_engine_event_after_local_cancel(&event) {
                    continue;
                }
            } else if !app.is_loading && ignore_stale_stream_event_while_idle(&event) {
                continue;
            }
            match event {
                EngineEvent::MessageStarted { .. } => {
                    // Assistant text starting after parallel tool work
                    // means the tool group is done. Flush the active
                    // cell first so the message lands BELOW the
                    // committed tool group (Codex pattern: streamed
                    // assistant content always flows after work).
                    app.flush_active_cell();
                    current_streaming_text.clear();
                    app.streaming_state.reset();
                    app.streaming_state.start_text(0, None);
                    app.streaming_message_index = None;
                }
                EngineEvent::MessageDelta { content, .. } => {
                    let sanitized = sanitize_stream_chunk(&content);
                    if sanitized.is_empty() {
                        continue;
                    }
                    // First delta of a fresh stream has no streaming
                    // cell yet; flush active so the tool group settles
                    // before the assistant prose appears below it.
                    if app.streaming_message_index.is_none() {
                        app.flush_active_cell();
                    }
                    current_streaming_text.push_str(&sanitized);
                    let index = ensure_streaming_assistant_history_cell(app);
                    app.streaming_state.push_content(0, &sanitized);
                    let committed = app.streaming_state.commit_text(0);
                    if !committed.is_empty() {
                        append_streaming_text(app, index, &committed);
                        transcript_batch_updated = true;
                    }
                }
                EngineEvent::MessageComplete { .. } => {
                    // #861 RC3: defensive drain of a still-active thinking
                    // entry. Normally `ThinkingComplete` arrives first and
                    // populates `last_reasoning` before we get here, but
                    // when the engine bursts events the channel can
                    // deliver `MessageComplete` first, in which case
                    // `last_reasoning.take()` below would be `None` and
                    // the thinking block would be dropped from
                    // `api_messages` — causing a DeepSeek HTTP 400 on the
                    // next turn (V4 thinking-mode requires
                    // `reasoning_content` replay). Inline-finalize the
                    // thinking entry here so this branch is order-
                    // independent.
                    if app.streaming_thinking_active_entry.is_some() {
                        if streaming_thinking::finalize_current(app) {
                            transcript_batch_updated = true;
                        }
                        streaming_thinking::stash_reasoning_buffer_into_last_reasoning(app);
                    }
                    let mut completed_message_index = None;
                    if let Some(index) = app.streaming_message_index.take() {
                        completed_message_index = Some(index);
                        let remaining = app.streaming_state.finalize_block_text(0);
                        if !remaining.is_empty() {
                            append_streaming_text(app, index, &remaining);
                        }
                        if let Some(HistoryCell::Assistant { streaming, .. }) =
                            app.history.get_mut(index)
                        {
                            *streaming = false;
                        }
                        // Streaming flag flipped — the cell's compact /
                        // transcript variants render slightly
                        // differently, so bump its revision so the cache
                        // refreshes this row only.
                        app.bump_history_cell(index);
                        transcript_batch_updated = true;
                    }

                    let thinking = app.last_reasoning.take();
                    let tool_uses = app.pending_tool_uses.drain(..).collect::<Vec<_>>();
                    let history_index = completed_message_index;

                    if app.translation_enabled
                        && !current_streaming_text.is_empty()
                        && crate::tui::translation::needs_translation(current_streaming_text)
                        && let Some(translation_client) = translation_client.as_ref()
                    {
                        app.status_message = Some(
                            crate::localization::tr(
                                app.ui_locale,
                                crate::localization::MessageId::TranslationInProgress,
                            )
                            .to_string(),
                        );
                        app.is_loading = true;
                        *pending_translations = pending_translations.saturating_add(1);
                        let tx = translation_tx.clone();
                        let client = translation_client.clone();
                        let original_text = current_streaming_text.clone();
                        let translation_model = app
                            .last_effective_model
                            .clone()
                            .unwrap_or_else(|| app.model.clone());
                        let target_language =
                            app.ui_locale.translation_target_name().to_string();
                        tokio::spawn(async move {
                            let translated = crate::tui::translation::translate_text(
                                &original_text,
                                &client,
                                &translation_model,
                                &target_language,
                            )
                            .await;
                            let _ = tx.send(TranslationEvent::AssistantMessage {
                                history_index,
                                original_text,
                                translated,
                                thinking,
                                tool_uses,
                            });
                        });
                    } else {
                        push_assistant_message(
                            app,
                            current_streaming_text.clone(),
                            thinking,
                            tool_uses,
                        );
                    }
                }
                EngineEvent::ThinkingStarted { .. } => {
                    // P2.3: thinking lives in the active cell so it groups
                    // visually with the tool calls that follow until the
                    // next assistant prose chunk flushes the group.
                    if streaming_thinking::start_block(app) {
                        transcript_batch_updated = true;
                    }
                    if app.translation_enabled {
                        let entry_idx = streaming_thinking::ensure_active_entry(app);
                        streaming_thinking::set_placeholder(app, entry_idx);
                        transcript_batch_updated = true;
                    }
                }
                EngineEvent::ThinkingDelta { content, .. } => {
                    let sanitized = sanitize_stream_chunk(&content);
                    if sanitized.is_empty() {
                        continue;
                    }
                    app.reasoning_buffer.push_str(&sanitized);
                    if app.reasoning_header.is_none() {
                        app.reasoning_header = extract_reasoning_header(&app.reasoning_buffer);
                    }

                    let entry_idx = streaming_thinking::ensure_active_entry(app);
                    app.streaming_state.push_content(0, &sanitized);
                    let committed = app.streaming_state.commit_text(0);
                    if !committed.is_empty() {
                        if app.translation_enabled {
                            streaming_thinking::set_placeholder(app, entry_idx);
                        } else {
                            streaming_thinking::append(app, entry_idx, &committed);
                        }
                        transcript_batch_updated = true;
                    }
                }
                EngineEvent::ThinkingComplete { .. } => {
                    if app.translation_enabled {
                        let original_thinking = app.reasoning_buffer.clone();
                        let _ = app.streaming_state.finalize_block_text(0);
                        let duration = app
                            .thinking_started_at
                            .take()
                            .map(|t| t.elapsed().as_secs_f32());
                        if streaming_thinking::finalize_active_entry(app, duration, "") {
                            transcript_batch_updated = true;
                        }
                        if !original_thinking.is_empty()
                            && crate::tui::translation::needs_translation(&original_thinking)
                            && let Some(translation_client) = translation_client.as_ref()
                        {
                            app.status_message = Some(
                                crate::localization::thinking_translation_in_progress(
                                    app.ui_locale,
                                )
                                .to_string(),
                            );
                            app.is_loading = true;
                            *pending_translations = pending_translations.saturating_add(1);
                            *pending_thinking_translations =
                                pending_thinking_translations.saturating_add(1);
                            let tx = translation_tx.clone();
                            let client = translation_client.clone();
                            let translation_model = app
                                .last_effective_model
                                .clone()
                                .unwrap_or_else(|| app.model.clone());
                            let placeholder =
                                crate::localization::thinking_translation_placeholder(
                                    app.ui_locale,
                                )
                                .to_string();
                            let target_language =
                                app.ui_locale.translation_target_name().to_string();
                            tokio::spawn(async move {
                                let translated = crate::tui::translation::translate_text(
                                    &original_thinking,
                                    &client,
                                    &translation_model,
                                    &target_language,
                                )
                                .await;
                                let _ = tx.send(TranslationEvent::Thinking {
                                    placeholder,
                                    translated,
                                });
                            });
                        } else {
                            let placeholder =
                                crate::localization::thinking_translation_placeholder(
                                    app.ui_locale,
                                );
                            streaming_thinking::replace_pending_translation(
                                app,
                                placeholder,
                                original_thinking,
                            );
                        }
                    } else if streaming_thinking::finalize_current(app) {
                        transcript_batch_updated = true;
                    }
                    streaming_thinking::stash_reasoning_buffer_into_last_reasoning(app);
                }
                EngineEvent::ToolCallStarted { id, name, input } => {
                    app.pending_tool_uses
                        .push((id.clone(), name.clone(), input.clone()));
                    // Note this dispatch so the next sub-agent `Started`
                    // mailbox envelope routes into the right card kind
                    // (delegate vs fanout).
                    if matches!(
                        name.as_str(),
                        "agent_open"
                            | "agent_spawn"
                            | "rlm_open"
                            | "rlm_eval"
                            | "rlm"
                            | "delegate"
                    ) {
                        app.pending_subagent_dispatch = Some(name.clone());
                        if matches!(name.as_str(), "rlm_open" | "rlm_eval" | "rlm") {
                            // New fanout invocation — children should
                            // group under a fresh card, not the
                            // previous fanout's leftover.
                            app.last_fanout_card_index = None;
                        }
                    }
                    handle_tool_call_started(app, &id, &name, &input);
                }
                EngineEvent::ToolCallComplete { id, name, result } => {
                    if name == "update_plan" {
                        app.plan_tool_used_in_turn = true;
                    }
                    let tool_content = match &result {
                        Ok(output) => sanitize_stream_chunk(
                            &crate::core::engine::compact_tool_result_for_context(
                                &app.model, &name, output,
                            ),
                        ),
                        Err(err) => sanitize_stream_chunk(&format!("Error: {err}")),
                    };
                    app.api_messages.push(Message {
                        role: "user".to_string(),
                        content: vec![ContentBlock::ToolResult {
                            tool_use_id: id.clone(),
                            content: tool_content,
                            is_error: None,
                            content_blocks: None,
                        }],
                    });
                    handle_tool_call_complete(app, &id, &name, &result);

                    // Immediately refresh the task panel sidebar when a
                    // tool that changes task state completes, so the
                    // Tasks panel stays in sync with tool execution
                    // rather than waiting up to 2.5 s for the periodic
                    // poll. Also merge shell jobs (#373).
                    if matches!(
                        name.as_str(),
                        "agent_open"
                            | "agent_spawn"
                            | "agent_close"
                            | "agent_cancel"
                            | "todo_write"
                            | "task_shell_start"
                            | "exec_shell"
                    ) {
                        refresh_active_task_panel(app, task_manager).await;
                        *last_task_refresh = Instant::now();
                    }
                    if matches!(
                        name.as_str(),
                        "agent_open"
                            | "agent_eval"
                            | "agent_close"
                            | "agent_cancel"
                            | "agent_wait"
                            | "agent_result"
                            | "agent_status"
                    ) {
                        let _ = engine_handle.send(Op::ListSubAgents).await;
                    }
                }
                EngineEvent::TurnStarted { turn_id } => {
                    app.suppress_stream_events_until_turn_complete = false;
                    app.is_loading = true;
                    app.offline_mode = false;
                    app.turn_error_posted = false;
                    app.dispatch_started_at = None;
                    current_streaming_text.clear();
                    app.streaming_state.reset();
                    app.streaming_message_index = None;
                    app.streaming_thinking_active_entry = None;
                    app.turn_started_at = Some(Instant::now());
                    // Discoverability hint for users who don't know how
                    // to interrupt a long-running turn (#1367). Only
                    // surface when the status_message slot is empty so
                    // we don't trample over a real transient message
                    // (e.g. "/queue saved", "Selection copied"); the
                    // hint then auto-clears as soon as anything else
                    // updates the slot.
                    if app.status_message.is_none() {
                        app.status_message = Some("Press Esc or Ctrl+C to cancel".to_string());
                    }
                    app.runtime_turn_id = Some(turn_id);
                    app.runtime_turn_status = Some("in_progress".to_string());
                    app.reasoning_buffer.clear();
                    app.reasoning_header = None;
                    app.last_reasoning = None;
                    app.pending_tool_uses.clear();
                    app.plan_tool_used_in_turn = false;
                    *last_status_frame = Instant::now();
                }
                EngineEvent::TurnComplete {
                    usage,
                    status,
                    error,
                } => {
                    let was_locally_cancelled = app.suppress_stream_events_until_turn_complete;
                    app.suppress_stream_events_until_turn_complete = false;
                    if !matches!(status, crate::core::events::TurnOutcomeStatus::Completed)
                        || draws_since_last_full_repaint >= PERIODIC_FULL_REPAINT_EVERY_N
                    {
                        *force_terminal_repaint = true;
                    }
                    // Finalize any in-flight tool group. Cancellation
                    // marks still-running entries as Failed so the user
                    // sees they were interrupted rather than the spinner
                    // hanging forever.
                    if matches!(
                        status,
                        crate::core::events::TurnOutcomeStatus::Interrupted
                            | crate::core::events::TurnOutcomeStatus::Failed
                    ) {
                        app.finalize_active_cell_as_interrupted();
                        // Also mark the streaming Assistant cell (if any)
                        // so partial reasoning/text isn't left with a
                        // permanent spinner. Idempotent with the
                        // optimistic call in the Esc handler.
                        app.finalize_streaming_assistant_as_interrupted();
                    } else {
                        app.flush_active_cell();
                    }
                    app.is_loading = false;
                    app.dispatch_started_at = None;
                    app.offline_mode = false;
                    app.streaming_state.reset();
                    if was_locally_cancelled {
                        current_streaming_text.clear();
                    }
                    // Capture elapsed before clearing turn_started_at so
                    // notifications can use the real wall-clock duration.
                    let turn_elapsed =
                        app.turn_started_at.map(|t| t.elapsed()).unwrap_or_default();
                    app.turn_started_at = None;
                    // Roll the just-finished turn's elapsed time into the
                    // cumulative session work-time (#448 follow-up). The
                    // footer's `worked Nh Mm` chip reads this so the
                    // label reflects actual model work, not idle
                    // uptime since launch.
                    app.cumulative_turn_duration =
                        app.cumulative_turn_duration.saturating_add(turn_elapsed);
                    // Stream lock applies per-turn; clear it so the next
                    // turn's chunks pull the view down again until the
                    // user opts out by scrolling up.
                    app.user_scrolled_during_stream = false;
                    app.runtime_turn_status = Some(match status {
                        crate::core::events::TurnOutcomeStatus::Completed => {
                            "completed".to_string()
                        }
                        crate::core::events::TurnOutcomeStatus::Interrupted => {
                            "interrupted".to_string()
                        }
                        crate::core::events::TurnOutcomeStatus::Failed => "failed".to_string(),
                    });
                    if matches!(
                        status,
                        crate::core::events::TurnOutcomeStatus::Interrupted
                            | crate::core::events::TurnOutcomeStatus::Failed
                    ) {
                        let _ = engine_handle.send(Op::ListSubAgents).await;
                    }
                    let turn_tokens = usage.input_tokens + usage.output_tokens;
                    app.session.total_tokens =
                        app.session.total_tokens.saturating_add(turn_tokens);
                    app.session.total_conversation_tokens = app
                        .session
                        .total_conversation_tokens
                        .saturating_add(turn_tokens);
                    app.session.total_input_tokens = app
                        .session
                        .total_input_tokens
                        .saturating_add(usage.input_tokens);
                    app.session.total_output_tokens = app
                        .session
                        .total_output_tokens
                        .saturating_add(usage.output_tokens);
                    // Only accumulate cache telemetry when reported.
                    if let Some(hit_tokens) = usage.prompt_cache_hit_tokens {
                        app.session.total_cache_hit_tokens = app
                            .session
                            .total_cache_hit_tokens
                            .saturating_add(hit_tokens);
                        let cache_miss = usage
                            .prompt_cache_miss_tokens
                            .unwrap_or_else(|| usage.input_tokens.saturating_sub(hit_tokens));
                        app.session.total_cache_miss_tokens = app
                            .session
                            .total_cache_miss_tokens
                            .saturating_add(cache_miss);
                    }
                    app.session.last_prompt_tokens = Some(usage.input_tokens);
                    app.session.last_completion_tokens = Some(usage.output_tokens);
                    app.session.last_prompt_cache_hit_tokens = usage.prompt_cache_hit_tokens;
                    app.session.last_prompt_cache_miss_tokens = usage.prompt_cache_miss_tokens;
                    app.session.last_reasoning_replay_tokens = usage.reasoning_replay_tokens;
                    app.push_turn_cache_record(TurnCacheRecord {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_hit_tokens: usage.prompt_cache_hit_tokens,
                        cache_miss_tokens: usage.prompt_cache_miss_tokens,
                        reasoning_replay_tokens: usage.reasoning_replay_tokens,
                        recorded_at: Instant::now(),
                    });
                    if let Some(error) = error {
                        // Only show "Turn failed:" in the composer status
                        // area when an EngineEvent::Error has NOT already
                        // posted the same message into the transcript.
                        // Otherwise the error appears twice: once in a
                        // HistoryCell and again as a redundant status line.
                        if !app.turn_error_posted {
                            app.status_message = Some(format!("Turn failed: {error}"));
                        }
                    }

                    // Update session cost
                    let pricing_model = if app.auto_model {
                        app.last_effective_model.as_deref().unwrap_or(&app.model)
                    } else {
                        &app.model
                    };
                    let turn_cost = crate::pricing::calculate_turn_cost_estimate_from_usage(
                        pricing_model,
                        &usage,
                    );
                    if let Some(cost) = turn_cost {
                        app.accrue_session_cost_estimate(cost);
                    }

                    // Emit OSC 9 / BEL desktop notification for long turns.
                    if status == crate::core::events::TurnOutcomeStatus::Completed
                        && let Some((method, threshold, include_summary)) =
                            notifications::settings(config)
                    {
                        let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
                        let msg = notifications::completed_turn_message(
                            app,
                            current_streaming_text,
                            include_summary,
                            turn_elapsed,
                            turn_cost,
                        );
                        crate::tui::notifications::notify_done(
                            method,
                            in_tmux,
                            &msg,
                            threshold,
                            turn_elapsed,
                        );
                    }

                    // Auto-save completed turn and clear crash checkpoint.
                    // Offloaded to the persistence actor so the UI
                    // stays responsive.
                    if let Ok(manager) = SessionManager::default_location() {
                        let session = build_session_snapshot(app, &manager);
                        app.current_session_id = Some(session.metadata.id.clone());
                        persistence_actor::persist(PersistRequest::SessionSnapshot(session));
                    }
                    persistence_actor::persist(PersistRequest::ClearCheckpoint);

                    if app.mode == AppMode::Plan
                        && app.plan_tool_used_in_turn
                        && !app.plan_prompt_pending
                        && app.queued_message_count() == 0
                        && app.queued_draft.is_none()
                    {
                        app.plan_prompt_pending = true;
                        app.add_message(HistoryCell::System {
                            content: plan_next_step_prompt(),
                        });
                        if app.view_stack.top_kind() != Some(ModalKind::PlanPrompt) {
                            app.view_stack.push(PlanPromptView::new());
                        }
                    }
                    app.plan_tool_used_in_turn = false;

                    // Legacy pending-steer recovery. Current keyboard
                    // handling keeps Esc as cancel-only, but older saved
                    // state may still carry pending steers.
                    if status == crate::core::events::TurnOutcomeStatus::Interrupted
                        && app.submit_pending_steers_after_interrupt
                    {
                        if let Some(merged) = merge_pending_steers(app) {
                            queued_to_send = Some(merged);
                        }
                    } else if status == crate::core::events::TurnOutcomeStatus::Failed
                        && !app.pending_steers.is_empty()
                    {
                        // Hard-fail recovery: if the engine failed before
                        // a clean Interrupted landed, demote pending
                        // steers to the visible queue so they're not
                        // silently lost. User can /queue to inspect.
                        for msg in app.drain_pending_steers() {
                            app.queue_message(msg);
                        }
                    }

                    if queued_to_send.is_none() {
                        queued_to_send = app.pop_queued_message();
                    }
                }
                EngineEvent::Error {
                    envelope,
                    recoverable: _,
                } => {
                    apply_engine_error_to_app(app, envelope);
                }
                EngineEvent::Status { message } => {
                    app.status_message = Some(message);
                }
                EngineEvent::SessionUpdated {
                    session_id,
                    messages,
                    system_prompt,
                    model,
                    workspace,
                } => {
                    app.current_session_id = Some(session_id);
                    app.api_messages = messages;
                    app.system_prompt = system_prompt;
                    if app.auto_model {
                        app.last_effective_model = Some(model);
                    } else {
                        app.set_model_selection(model);
                    }
                    app.update_model_compaction_budget();
                    app.workspace = workspace;
                    if (app.is_loading || app.is_compacting)
                        && let Ok(manager) = SessionManager::default_location()
                    {
                        let session = build_session_snapshot(app, &manager);
                        app.session_title = Some(session.metadata.title.clone());
                        persistence_actor::persist(PersistRequest::Checkpoint(session));
                    } else if app.session_title.is_none() {
                        // First turn on a brand-new session: persist hasn't fired yet so
                        // read the title from the session file if it already exists,
                        // otherwise fall back to deriving from messages.
                        let persisted = app
                            .current_session_id
                            .as_deref()
                            .and_then(|id| {
                                SessionManager::default_location()
                                    .ok()?
                                    .load_session(id)
                                    .ok()
                            })
                            .map(|s| s.metadata.title);
                        app.session_title =
                            persisted.or_else(|| derive_session_title(&app.api_messages));
                    }
                }
                EngineEvent::CompactionStarted { message, .. } => {
                    app.is_compacting = true;
                    app.status_message = Some(message);
                }
                EngineEvent::CompactionCompleted { message, .. } => {
                    app.is_compacting = false;
                    app.status_message = Some(message);
                }
                EngineEvent::CompactionFailed { message, .. } => {
                    app.is_compacting = false;
                    app.status_message = Some(message);
                }
                EngineEvent::CycleAdvanced { from, to, briefing } => {
                    // Mirror the engine-side counter on the UI app state
                    // so the sidebar / slash commands stay in sync, and
                    // record the briefing so `/cycle <n>` can show it.
                    app.cycle_count = to;
                    let briefing_tokens = briefing.token_estimate;
                    app.cycle_briefings.push(briefing);
                    let separator = format!(
                        "─── cycle {from} → {to}  (briefing: {briefing_tokens} tokens) ───"
                    );
                    app.add_message(HistoryCell::System { content: separator });
                    app.status_message = Some(format!(
                        "↻ context refreshed (cycle {from} → {to}, briefing: {briefing_tokens} tokens carried)"
                    ));
                }
                EngineEvent::CoherenceState { state, .. } => {
                    app.coherence_state = state;
                }
                EngineEvent::PrefixCacheChange {
                    description,
                    stability_pct,
                    changed,
                    pinned_combined_hash,
                    ..
                } => {
                    app.prefix_checks_total = app.prefix_checks_total.saturating_add(1);
                    app.prefix_stability_pct = Some(stability_pct);
                    app.last_pinned_prefix_hash =
                        (!pinned_combined_hash.is_empty()).then_some(pinned_combined_hash);
                    if changed {
                        app.prefix_change_count = app.prefix_change_count.saturating_add(1);
                        if !description.is_empty() {
                            app.last_prefix_change_desc = Some(description);
                        }
                    }
                }
                EngineEvent::CapacityDecision { .. } => {
                    // Telemetry-only event. Surface actual interventions and failures
                    // instead of replacing the footer with no-op guardrail chatter.
                }
                EngineEvent::CapacityIntervention {
                    action,
                    before_prompt_tokens,
                    after_prompt_tokens,
                    ..
                } => {
                    app.status_message = Some(format!(
                        "Capacity intervention: {action} (~{before_prompt_tokens} -> ~{after_prompt_tokens} tokens)"
                    ));
                }
                EngineEvent::CapacityMemoryPersistFailed { action, error, .. } => {
                    app.status_message = Some(format!(
                        "Capacity memory persist failed ({action}): {error}"
                    ));
                }
                EngineEvent::PauseEvents { ack } => {
                    if !event_broker.is_paused() {
                        pause_terminal(
                            terminal,
                            app.use_alt_screen,
                            app.use_mouse_capture,
                            app.use_bracketed_paste,
                        )?;
                        event_broker.pause_events();
                        *terminal_paused_at = Some(Instant::now());
                    }
                    if let Some(ack) = ack {
                        ack.notify_one();
                    }
                }
                EngineEvent::ResumeEvents => {
                    if event_broker.is_paused() {
                        resume_terminal(
                            terminal,
                            app.use_alt_screen,
                            app.use_mouse_capture,
                            app.use_bracketed_paste,
                            app.synchronized_output_enabled,
                        )?;
                        event_broker.resume_events();
                        *terminal_paused_at = None;
                    }
                }
                EngineEvent::AgentSpawned { id, prompt } => {
                    let prompt_summary = summarize_tool_output(&prompt);
                    app.agent_progress
                        .insert(id.clone(), format!("starting: {prompt_summary}"));
                    if app.agent_activity_started_at.is_none() {
                        app.agent_activity_started_at = Some(Instant::now());
                    }
                    app.status_message =
                        Some(format!("Sub-agent {id} starting: {prompt_summary}"));
                    let _ = engine_handle.send(Op::ListSubAgents).await;
                }
                EngineEvent::AgentProgress { id, status } => {
                    let display = friendly_subagent_progress(app, &id, &status);
                    if is_noisy_subagent_progress(&status) {
                        app.agent_progress
                            .entry(id.clone())
                            .or_insert_with(|| display.clone());
                    } else {
                        app.agent_progress.insert(id.clone(), display.clone());
                    }
                    if app.agent_activity_started_at.is_none() {
                        app.agent_activity_started_at = Some(Instant::now());
                    }
                    app.status_message = Some(format!("Sub-agent {id}: {display}"));
                }
                EngineEvent::AgentComplete { id, result } => {
                    let subagent_elapsed = app
                        .agent_activity_started_at
                        .or(app.turn_started_at)
                        .map(|started| started.elapsed())
                        .unwrap_or_default();
                    let has_other_running_subagents =
                        app.agent_progress.keys().any(|agent_id| agent_id != &id)
                            || app.subagent_cache.iter().any(|agent| {
                                agent.agent_id != id
                                    && matches!(agent.status, SubAgentStatus::Running)
                            });
                    app.agent_progress.remove(&id);
                    app.status_message = Some(format!(
                        "Sub-agent {id} completed: {}",
                        summarize_tool_output(&result)
                    ));
                    let should_recapture_terminal =
                        !has_other_running_subagents && app.use_alt_screen;
                    if !has_other_running_subagents
                        && let Some((method, threshold, include_summary)) =
                            notifications::settings(config)
                    {
                        let in_tmux = std::env::var("TMUX").is_ok_and(|v| !v.is_empty());
                        let msg = notifications::subagent_completion_message(
                            &id,
                            &result,
                            include_summary,
                            subagent_elapsed,
                        );
                        crate::tui::notifications::notify_done(
                            method,
                            in_tmux,
                            &msg,
                            threshold,
                            subagent_elapsed,
                        );
                    }
                    if should_recapture_terminal {
                        resume_terminal(
                            terminal,
                            app.use_alt_screen,
                            app.use_mouse_capture,
                            app.use_bracketed_paste,
                            app.synchronized_output_enabled,
                        )?;
                        event_broker.resume_events();
                        *terminal_paused_at = None;
                        app.needs_redraw = true;
                    }
                    let _ = engine_handle.send(Op::ListSubAgents).await;
                }
                EngineEvent::AgentList { agents } => {
                    let mut sorted = agents.clone();
                    sort_subagents_in_place(&mut sorted);
                    sorted.retain(|a| !a.from_prior_session);
                    app.subagent_cache = sorted.clone();
                    reconcile_subagent_activity_state(app);
                    let view_agents = subagent_view_agents(app, &sorted);
                    if app.view_stack.update_subagents(&view_agents) {
                        app.status_message =
                            Some(format!("Sub-agents: {} total", view_agents.len()));
                    }
                    // Individual spawn/complete events already log to history;
                    // full list available via /agents command.
                }
                EngineEvent::SubAgentMailbox { seq, message } => {
                    handle_subagent_mailbox(app, seq, &message);
                    transcript_batch_updated = true;
                }
                EngineEvent::ApprovalRequired {
                    id,
                    tool_name,
                    description,
                    approval_key,
                    approval_grouping_key,
                } => {
                    let session_approved =
                        is_session_approved_for_tool(app, &tool_name, &approval_grouping_key);
                    let session_denied = is_session_denied_for_key(app, &approval_key);
                    if session_denied {
                        // The user already said no to this exact tool /
                        // approval key in this session; auto-deny so the
                        // model's retry loop doesn't keep re-prompting
                        // (#360).
                        log_sensitive_event(
                            "tool.approval.auto_deny_session",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "approval_key": approval_key,
                                "session_id": app.current_session_id,
                            }),
                        );
                        let _ = engine_handle.deny_tool_call(id.clone()).await;
                    } else if session_approved || app.approval_mode == ApprovalMode::Auto {
                        log_sensitive_event(
                            "tool.approval.auto_approve",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "approval_key": approval_key,
                                "session_id": app.current_session_id,
                                "mode": app.mode.label(),
                            }),
                        );
                        let _ = engine_handle.approve_tool_call(id.clone()).await;
                    } else if app.approval_mode == ApprovalMode::Never {
                        log_sensitive_event(
                            "tool.approval.auto_deny",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "session_id": app.current_session_id,
                                "mode": app.mode.label(),
                            }),
                        );
                        let _ = engine_handle.deny_tool_call(id.clone()).await;
                        app.status_message =
                            Some(format!("Blocked tool '{tool_name}' (approval_mode=never)"));
                    } else {
                        let tool_input = app
                            .pending_tool_uses
                            .iter()
                            .find(|(tool_id, _, _)| tool_id == &id)
                            .map(|(_, _, input)| input.clone())
                            .unwrap_or_else(|| serde_json::json!({}));

                        if tool_name == "apply_patch" {
                            maybe_add_patch_preview(app, &tool_input);
                        }

                        // Create approval request and show overlay
                        let request = ApprovalRequest::new(
                            &id,
                            &tool_name,
                            &description,
                            &tool_input,
                            &approval_key,
                        );
                        log_sensitive_event(
                            "tool.approval.prompted",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "description": description,
                                "session_id": app.current_session_id,
                                "mode": app.mode.label(),
                            }),
                        );
                        app.view_stack
                            .push(ApprovalView::new_for_locale(request, app.ui_locale));
                        app.status_message = Some(format!(
                            "Approval required for '{tool_name}': {description}"
                        ));
                    }
                }
                EngineEvent::UserInputRequired { id, request } => {
                    app.view_stack.push(UserInputView::new(id.clone(), request));
                    app.status_message = Some(
                        "Action required: answer the popup with 1-4, arrows, or Enter"
                            .to_string(),
                    );
                }
                EngineEvent::ToolCallProgress { id, output } => {
                    app.status_message =
                        Some(format!("Tool {id}: {}", summarize_tool_output(&output)));
                }
                EngineEvent::ElevationRequired {
                    tool_id,
                    tool_name,
                    command,
                    denial_reason,
                    blocked_network,
                    blocked_write,
                } => {
                    // In YOLO mode, auto-elevate to full access
                    if app.approval_mode == ApprovalMode::Auto {
                        log_sensitive_event(
                            "tool.sandbox.auto_elevate",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "tool_id": tool_id,
                                "reason": denial_reason,
                                "session_id": app.current_session_id,
                            }),
                        );
                        app.add_message(HistoryCell::System {
                            content: format!(
                                "Sandbox denied {tool_name}: {denial_reason} - auto-elevating to full access"
                            ),
                        });
                        // Auto-elevate to full access (no sandbox)
                        let policy = crate::sandbox::SandboxPolicy::DangerFullAccess;
                        let _ = engine_handle.retry_tool_with_policy(tool_id, policy).await;
                    } else {
                        log_sensitive_event(
                            "tool.sandbox.prompt_elevation",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "tool_id": tool_id,
                                "reason": denial_reason,
                                "session_id": app.current_session_id,
                            }),
                        );
                        // Show elevation dialog
                        let request = ElevationRequest::for_shell(
                            &tool_id,
                            command.as_deref().unwrap_or(&tool_name),
                            &denial_reason,
                            blocked_network,
                            blocked_write,
                        );
                        app.view_stack.push(ElevationView::new(request));
                        app.status_message =
                            Some(format!("Sandbox blocked {tool_name}: {denial_reason}"));
                    }
                }
            }
        }
    }
    Ok(EngineEventPollResult {
        received_engine_event,
        transcript_batch_updated,
        queued_to_send,
    })
}
