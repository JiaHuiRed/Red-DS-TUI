//! Keyboard-event handling for the TUI event loop.
//!
//! Extracted from `run_event_loop` (crates/tui/src/tui/ui.rs) to keep the
//! main loop small. Two entry points:
//!
//! - `handle_onboarding_key` — the first-run onboarding state machine.
//! - `handle_key_event` — every other key binding (help, palettes, composer,
//!   vim mode, transcript navigation, …).
//!
//! Both return an action enum so the caller can preserve the original
//! `continue` / `return Ok(())` semantics without the code being inline.

use std::time::Instant;

use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::config::Config;
use crate::config_ui::WebConfigSession;
use crate::core::engine::{EngineHandle, spawn_engine};
use crate::core::ops::Op;
use crate::tui::app::{App, AppMode, OnboardingState};
use crate::tui::command_palette::{
    CommandPaletteView, build_entries as build_command_palette_entries,
};
use crate::tui::composer_ui::{
    select_next_slash_menu_entry, select_previous_slash_menu_entry,
};
use crate::tui::onboarding;
use crate::tui::session_picker::SessionPickerView;
use crate::task_manager::SharedTaskManager;
use crate::tui::views::{HelpView, ModalKind};

use super::{
    AppTerminal, CtrlCDisposition, SLASH_MENU_LIMIT, MENTION_MENU_LIMIT, SearchDirection,
    apply_slash_menu_selection, build_engine_config, build_queued_message, copy_active_selection,
    copy_focused_cell, copy_thinking_cell, count_user_history_cells, ctrl_c_disposition,
    detail_target_cell_index, execute_command_input, handle_composer_history_arrow,
    handle_history_search_key, handle_memory_quick_add, handle_plan_choice, handle_view_events,
    is_composer_newline_key, is_memory_quick_add, is_word_cursor_modifier,
    jump_to_adjacent_tool_cell, looks_like_slash_command_input, mark_active_turn_cancelled_locally,
    next_escape_action, open_activity_detail_pager, open_backtrack_overlay, open_context_inspector,
    open_pager_for_last_message, open_pager_for_selection, open_shell_control,
    open_tool_details_pager, queue_current_draft_for_next_turn, steer_user_message,
    submit_or_steer_message, toggle_live_transcript_overlay, try_autocomplete_slash_command,
};

/// What the caller should do after an onboarding key was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OnboardingAction {
    /// Key was handled; the event loop should keep processing.
    Handled,
    /// The user asked to exit; the event loop must return.
    Shutdown,
}

/// Handle one key while the onboarding flow is active.
///
/// Mirrors the inline `match key.code` previously inside `run_event_loop`.
/// `engine_handle` is `&mut` because the API-key submit path recreates the
/// engine so it picks up the freshly saved key.
#[allow(clippy::too_many_lines)]
pub(super) async fn handle_onboarding_key(
    app: &mut App,
    config: &mut Config,
    engine_handle: &mut EngineHandle,
    key: KeyEvent,
) -> Result<OnboardingAction> {
    match key.code {
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let _ = engine_handle.send(Op::Shutdown).await;
            return Ok(OnboardingAction::Shutdown);
        }
        KeyCode::Esc if app.onboarding == OnboardingState::ApiKey => {
            app.onboarding = OnboardingState::Welcome;
            app.api_key_input.clear();
            app.api_key_cursor = 0;
            app.status_message = None;
        }
        KeyCode::Esc if app.onboarding == OnboardingState::Language => {
            app.onboarding = OnboardingState::Welcome;
            app.status_message = None;
        }
        // Language picker hotkeys select + persist (#566).
        //
        // Note: this used to be a single match-guard with `&& let`,
        // but `if_let_guard` is a nightly-only feature on Rust
        // before 1.94. Rewriting as a plain guard + nested `if let`
        // keeps `cargo install` working on stable.
        KeyCode::Char(c) if app.onboarding == OnboardingState::Language && c.is_ascii_digit() => {
            if let Some((_, tag, _, _)) = onboarding::language::LANGUAGE_OPTIONS
                .iter()
                .find(|(hotkey, _, _, _)| *hotkey == c)
            {
                match app.set_locale_from_onboarding(tag) {
                    Ok(()) => {
                        app.push_status_toast(
                            format!("Language set to {tag}"),
                            crate::tui::app::StatusToastLevel::Info,
                            Some(2_500),
                        );
                        onboarding::advance_onboarding_after_language(app);
                    }
                    Err(err) => {
                        app.status_message = Some(format!("Failed to save locale: {err}"));
                    }
                }
            }
        }
        KeyCode::Enter => match app.onboarding {
            OnboardingState::Welcome => {
                onboarding::advance_onboarding_from_welcome(app);
            }
            OnboardingState::Language => {
                // Enter without a digit pick keeps the existing
                // setting (which defaults to "auto").
                onboarding::advance_onboarding_after_language(app);
            }
            OnboardingState::ApiKey => {
                let key = app.api_key_input.trim().to_string();
                if let onboarding::ApiKeyValidation::Reject(message) =
                    onboarding::validate_api_key_for_onboarding(&key)
                {
                    app.status_message = Some(message);
                    return Ok(OnboardingAction::Handled);
                }
                match app.submit_api_key() {
                    Ok(saved) => {
                        // Surface where the key landed so the
                        // user can verify the shared config
                        // file path before the welcome
                        // screen advances. The toast queue
                        // outlives the onboarding state
                        // transition, so it stays visible on
                        // the next screen too.
                        app.push_status_toast(
                            format!("API key saved to {}", saved.describe()),
                            crate::tui::app::StatusToastLevel::Info,
                            Some(4_000),
                        );
                        app.status_message = None;
                        // Recreate the engine so it picks up the newly saved key
                        // without requiring a full process restart.
                        let _ = engine_handle.send(Op::Shutdown).await;
                        // Stamp the new key on the long-lived
                        // `Config` reference so any future clone
                        // (e.g. a subsequent /provider switch)
                        // sees it; the explicit-override path
                        // in `deepseek_api_key` (#343) makes
                        // this win immediately.
                        config.api_key = Some(key.clone());
                        let mut refreshed_config = config.clone();
                        refreshed_config.api_key = Some(key);
                        let engine_config = build_engine_config(app, &refreshed_config);
                        *engine_handle = spawn_engine(engine_config, &refreshed_config);
                        app.offline_mode = false;
                        app.api_key_env_only = false;

                        if !app.api_messages.is_empty() {
                            let _ = engine_handle
                                .send(Op::SyncSession {
                                    session_id: app.current_session_id.clone(),
                                    messages: app.api_messages.clone(),
                                    system_prompt: app.system_prompt.clone(),
                                    system_prompt_override: false,
                                    model: app.model.clone(),
                                    workspace: app.workspace.clone(),
                                })
                                .await;
                        }

                        onboarding::advance_onboarding_after_language(app);
                    }
                    Err(e) => {
                        app.status_message = Some(e.to_string());
                    }
                }
            }
            OnboardingState::TrustDirectory => {}
            OnboardingState::Tips => {
                app.finish_onboarding();
            }
            OnboardingState::None => {}
        },
        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('1')
            if app.onboarding == OnboardingState::TrustDirectory =>
        {
            match onboarding::mark_trusted(&app.workspace) {
                Ok(_) => {
                    app.trust_mode = true;
                    app.status_message = None;
                    if app.onboarding_workspace_trust_gate {
                        app.onboarding_workspace_trust_gate = false;
                        app.onboarding = OnboardingState::None;
                    } else {
                        app.onboarding = OnboardingState::Tips;
                    }
                }
                Err(err) => {
                    app.status_message = Some(format!("Failed to trust workspace: {err}"));
                }
            }
        }
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('2')
            if app.onboarding == OnboardingState::TrustDirectory =>
        {
            let _ = engine_handle.send(Op::Shutdown).await;
            return Ok(OnboardingAction::Shutdown);
        }
        KeyCode::Backspace if app.onboarding == OnboardingState::ApiKey => {
            app.delete_api_key_char();
            onboarding::sync_api_key_validation_status(app, false);
        }
        KeyCode::Char('h')
            if key_shortcuts_is_ctrl_h_backspace(&key) && app.onboarding == OnboardingState::ApiKey =>
        {
            app.delete_api_key_char();
            onboarding::sync_api_key_validation_status(app, false);
        }
        _ if key_shortcuts_is_paste_shortcut(&key) && app.onboarding == OnboardingState::ApiKey => {
            // Cmd+V / Ctrl+V paste (bracketed paste handled above)
            app.paste_api_key_from_clipboard();
            onboarding::sync_api_key_validation_status(app, false);
        }
        KeyCode::Char(c)
            if app.onboarding == OnboardingState::ApiKey
                && key_shortcuts_is_text_input_key(&key) =>
        {
            app.insert_api_key_char(c);
            onboarding::sync_api_key_validation_status(app, false);
        }
        _ => {}
    }
    Ok(OnboardingAction::Handled)
}

/// What the caller should do after a regular key was handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum KeyEventAction {
    /// Key was consumed; the event loop should `continue`.
    Handled,
    /// A command requested the event loop to return.
    Return,
    /// Nothing consumed the key; caller continues as usual.
    Continue,
}

/// Handle a `Press` key event for the main (non-onboarding) flow.
///
/// Mirrors the inline keyboard handling previously inside `run_event_loop`.
/// Returns `KeyEventAction` so the caller can preserve `continue` /
/// `return Ok(())` semantics.
#[allow(clippy::too_many_lines, clippy::too_many_arguments)]
pub(super) async fn handle_key_event(
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &mut Config,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    web_config_session: &mut Option<WebConfigSession>,
    key: KeyEvent,
    current_streaming_text: &mut String,
) -> Result<KeyEventAction> {
    if key.code == KeyCode::F(1) {
        if app.view_stack.top_kind() == Some(ModalKind::Help) {
            app.view_stack.pop();
        } else {
            app.view_stack.push(HelpView::new_for_locale(app.ui_locale));
        }
        return Ok(KeyEventAction::Handled);
    }

    if key.code == KeyCode::Char('/') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.view_stack.top_kind() == Some(ModalKind::Help) {
            app.view_stack.pop();
        } else {
            app.view_stack.push(HelpView::new_for_locale(app.ui_locale));
        }
        return Ok(KeyEventAction::Handled);
    }

    if key.code == KeyCode::Char('k') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if app.view_stack.is_empty()
            && app.sidebar_focus == crate::tui::app::SidebarFocus::Tasks
            && app
                .task_panel
                .iter()
                .any(|task| task.id.starts_with("shell_") && task.status == "running")
        {
            app.input = "/jobs cancel-all".to_string();
            app.cursor_position = app.input.len();
            app.status_message = Some("Press Enter to kill all running shell jobs".to_string());
            return Ok(KeyEventAction::Handled);
        }
        // When the composer is the active input target (no modal/pager
        // intercepting keys), Ctrl+K performs an emacs-style kill to
        // end-of-line. If the kill is a no-op (cursor at end of empty
        // input), fall through to the existing command palette.
        if app.view_stack.is_empty() && app.kill_to_end_of_line() {
            return Ok(KeyEventAction::Handled);
        }
        app.view_stack
            .push(CommandPaletteView::new(build_command_palette_entries(
                app.ui_locale,
                &app.skills_dir,
                &app.workspace,
                &app.mcp_config_path,
                app.mcp_snapshot.as_ref(),
            )));
        return Ok(KeyEventAction::Handled);
    }

    // Shifted shortcuts toggle the file-tree pane. Keep plain Ctrl+E
    // reserved for the composer end-of-line binding used by shells.
    if key_shortcuts_is_file_tree_toggle_shortcut(&key) {
        if let Some(_state) = app.file_tree.as_mut() {
            // File tree visible → hide it.
            app.file_tree = None;
            app.status_message = Some("File tree closed".to_string());
        } else {
            // Build the file tree from the current workspace.
            let state = crate::tui::file_tree::FileTreeState::new(&app.workspace);
            app.file_tree = Some(state);
            app.status_message = Some(
                "File tree: \u{2191}/\u{2193} navigate  Enter select  Esc close".to_string(),
            );
        }
        app.needs_redraw = true;
        return Ok(KeyEventAction::Handled);
    }

    // Ctrl+P opens the fuzzy file-picker overlay. Bound only when the
    // composer is focused (no other modal on top of the stack) and the
    // engine is not actively streaming a turn.
    if key.code == KeyCode::Char('p')
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && app.view_stack.is_empty()
        && !app.is_loading
    {
        crate::tui::file_picker_relevance::open_file_picker(app);
        return Ok(KeyEventAction::Handled);
    }

    if matches!(key.code, KeyCode::Char('b') | KeyCode::Char('B'))
        && key.modifiers.contains(KeyModifiers::CONTROL)
        && app.view_stack.is_empty()
    {
        open_shell_control(app);
        return Ok(KeyEventAction::Handled);
    }

    if matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
        && app.view_stack.is_empty()
    {
        open_context_inspector(app);
        return Ok(KeyEventAction::Handled);
    }

    if !app.view_stack.is_empty() {
        let events = app.view_stack.handle_key(key);
        app.needs_redraw = true;
        if handle_view_events(
            terminal,
            app,
            config,
            task_manager,
            engine_handle,
            web_config_session,
            events,
        )
        .await?
        {
            return Ok(KeyEventAction::Return);
        }
        return Ok(KeyEventAction::Handled);
    }

    // File-tree navigation: intercept keys when the file-tree pane is
    // visible so Up/Down/Enter/Esc operate on the tree rather than
    // falling through to composer or modal handlers.
    if app.file_tree.is_some() {
        match key.code {
            KeyCode::Up => {
                if let Some(state) = app.file_tree.as_mut() {
                    state.cursor_up();
                }
                app.needs_redraw = true;
                return Ok(KeyEventAction::Handled);
            }
            KeyCode::Down => {
                if let Some(state) = app.file_tree.as_mut() {
                    state.cursor_down();
                }
                app.needs_redraw = true;
                return Ok(KeyEventAction::Handled);
            }
            KeyCode::Enter => {
                if let Some(state) = app.file_tree.as_mut() {
                    if let Some(rel_path) = state.activate() {
                        // Insert @path into the composer.
                        let path_str = rel_path.to_string_lossy().to_string();
                        app.status_message = Some(format!("Attached @{path_str}"));
                        app.insert_str(&format!("@{} ", path_str));
                    } else {
                        // Directory was expanded/collapsed; rebuild.
                        app.needs_redraw = true;
                    }
                }
                return Ok(KeyEventAction::Handled);
            }
            KeyCode::Esc => {
                app.file_tree = None;
                app.status_message = Some("File tree closed".to_string());
                app.needs_redraw = true;
                return Ok(KeyEventAction::Handled);
            }
            _ => {}
        }
    }

    if app.is_history_search_active() {
        handle_history_search_key(app, key);
        return Ok(KeyEventAction::Handled);
    }

    if matches!(key.code, KeyCode::Char('r') | KeyCode::Char('R'))
        && key.modifiers.contains(KeyModifiers::ALT)
        && !key.modifiers.contains(KeyModifiers::CONTROL)
        && !key.modifiers.contains(KeyModifiers::SUPER)
    {
        app.start_history_search();
        return Ok(KeyEventAction::Handled);
    }

    let now = Instant::now();
    app.flush_paste_burst_if_enabled(now);

    // On Windows, AltGr is delivered as `Ctrl+Alt`; treat
    // AltGr-typed chars (e.g. European layouts producing `@`, `\`,
    // `|`) as plain text rather than swallowing them as a modified
    // shortcut. `key_hint::has_ctrl_or_alt` filters AltGr out.
    let has_ctrl_alt_or_super = crate::tui::widgets::key_hint::has_ctrl_or_alt(key.modifiers)
        || key.modifiers.contains(KeyModifiers::SUPER);
    let is_plain_char = matches!(key.code, KeyCode::Char(_)) && !has_ctrl_alt_or_super;
    let is_enter = matches!(key.code, KeyCode::Enter);

    if key_shortcuts_is_macos_option_v_legacy_key(&key) {
        open_tool_details_pager(app);
        return Ok(KeyEventAction::Handled);
    }

    if !is_plain_char
        && !is_enter
        && let Some(pending) = app.flush_paste_burst_before_modified_input_if_enabled()
    {
        app.insert_str(&pending);
    }

    if (is_plain_char || is_enter) && crate::tui::paste::handle_paste_burst_key(app, &key, now) {
        return Ok(KeyEventAction::Handled);
    }

    let slash_menu_entries = crate::tui::slash_menu::visible_slash_menu_entries(app, SLASH_MENU_LIMIT);
    let slash_menu_open = !slash_menu_entries.is_empty();
    if slash_menu_open && app.slash_menu_selected >= slash_menu_entries.len() {
        app.slash_menu_selected = slash_menu_entries.len().saturating_sub(1);
    }
    let mention_menu_entries =
        crate::tui::file_mention::visible_mention_menu_entries(app, MENTION_MENU_LIMIT);
    let mention_menu_open = !mention_menu_entries.is_empty();
    if mention_menu_open && app.mention_menu_selected >= mention_menu_entries.len() {
        app.mention_menu_selected = mention_menu_entries.len().saturating_sub(1);
    }

    // Cancel a pending Esc-Esc prime as soon as any non-Esc key
    // arrives. Without this the prime would hang around for the
    // rest of the session and the user's next genuine Esc would
    // suddenly skip straight into the backtrack overlay.
    if !matches!(key.code, KeyCode::Esc)
        && matches!(
            app.backtrack.phase,
            crate::tui::backtrack::BacktrackPhase::Primed
        )
    {
        app.backtrack.reset();
    }

    // Global keybindings
    match key.code {
        KeyCode::Enter
            if app.input.is_empty()
                && app.viewport.transcript_selection.is_active()
                && open_pager_for_selection(app) =>
        {
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('l')
            if crate::tui::key_shortcuts::alt_nav_modifiers(key.modifiers)
                && app.input.is_empty()
                && open_pager_for_last_message(app) =>
        {
            return Ok(KeyEventAction::Handled);
        }
        // Bare `v` / `V` no longer opens the tool-details pager — that
        // path is owned exclusively by `Alt+V` at the lower arm, so
        // the letter `v` is freely usable as the first character of
        // a message. `details_shortcut_modifiers` previously allowed
        // empty/Shift here, eating the keystroke on empty composers.
        KeyCode::Char('o')
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && app.input.is_empty()
                && open_activity_detail_pager(app) =>
        {
            return Ok(KeyEventAction::Handled);
        }
        // Space toggles fold/unfold of the focused thinking block
        // when the composer is empty. For thinking cells, toggles
        // between summary and full content; for other cells, toggles
        // visibility (#1972, #2348).
        KeyCode::Char(' ')
            if key.modifiers == KeyModifiers::NONE && app.input.is_empty() =>
        {
            if let Some(idx) = detail_target_cell_index(app) {
                let is_thinking = app
                    .history
                    .get(idx)
                    .is_some_and(|c| matches!(c, crate::tui::history::HistoryCell::Thinking { .. }));
                if is_thinking {
                    if app.folded_thinking.contains(&idx) {
                        app.folded_thinking.remove(&idx);
                        app.status_message = Some("Thinking block expanded".to_string());
                    } else {
                        app.folded_thinking.insert(idx);
                        app.status_message = Some("Thinking block folded".to_string());
                    }
                } else if app.collapsed_cells.contains(&idx) {
                    app.collapsed_cells.remove(&idx);
                    app.status_message = Some("Cell expanded".to_string());
                } else {
                    app.collapsed_cells.insert(idx);
                    app.status_message = Some("Cell collapsed".to_string());
                }
                app.mark_history_updated();
                app.needs_redraw = true;
            }
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('t') | KeyCode::Char('T')
            if key.modifiers == KeyModifiers::CONTROL =>
        {
            toggle_live_transcript_overlay(app);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('1') if key.modifiers.contains(KeyModifiers::ALT) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                app.set_sidebar_focus(crate::tui::app::SidebarFocus::Work);
                app.status_message = Some("Sidebar focus: work".to_string());
            } else {
                app.set_mode(AppMode::Plan);
            }
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('2') if key.modifiers.contains(KeyModifiers::ALT) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                app.set_sidebar_focus(crate::tui::app::SidebarFocus::Tasks);
                app.status_message = Some("Sidebar focus: tasks".to_string());
            } else {
                app.set_mode(AppMode::Agent);
            }
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('3') if key.modifiers.contains(KeyModifiers::ALT) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                app.set_sidebar_focus(crate::tui::app::SidebarFocus::Agents);
                app.status_message = Some("Sidebar focus: agents".to_string());
            } else {
                app.set_mode(AppMode::Yolo);
            }
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('4') if key.modifiers.contains(KeyModifiers::ALT) => {
            super::apply_alt_4_shortcut(app, key.modifiers);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('!') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_sidebar_focus(crate::tui::app::SidebarFocus::Work);
            app.status_message = Some("Sidebar focus: work".to_string());
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('@') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_sidebar_focus(crate::tui::app::SidebarFocus::Tasks);
            app.status_message = Some("Sidebar focus: tasks".to_string());
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('#') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_sidebar_focus(crate::tui::app::SidebarFocus::Agents);
            app.status_message = Some("Sidebar focus: agents".to_string());
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('$') | KeyCode::Char('%')
            if key.modifiers.contains(KeyModifiers::ALT) =>
        {
            app.set_sidebar_focus(crate::tui::app::SidebarFocus::Context);
            app.status_message = Some("Sidebar focus: context".to_string());
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char(')') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_sidebar_focus(crate::tui::app::SidebarFocus::Auto);
            app.status_message = Some("Sidebar focus: auto".to_string());
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('0') if key.modifiers.contains(KeyModifiers::ALT) => {
            super::apply_alt_0_shortcut(app, key.modifiers);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Scope the picker to the current workspace so Ctrl+R
            // never restores a different project's history by
            // surprise (#1395). Press `a` inside the picker to
            // broaden to every saved session.
            app.view_stack.push(SessionPickerView::new(&app.workspace));
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('c') | KeyCode::Char('C')
            if crate::tui::key_shortcuts::is_copy_shortcut(&key) =>
        {
            copy_active_selection(app);
        }
        KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Four behaviors layered on Ctrl+C in priority order — see
            // `CtrlCDisposition` for the unit-tested decision table.
            // 1. selection active → copy + clear (Windows convention,
            //    #1337); 2. turn in flight → cancel; 3. quit-armed →
            //    exit; 4. otherwise → arm the 2-second exit prompt.
            match ctrl_c_disposition(app) {
                CtrlCDisposition::CopySelection => {
                    copy_active_selection(app);
                    app.viewport.transcript_selection.clear();
                }
                CtrlCDisposition::CancelTurn => {
                    engine_handle.cancel();
                    mark_active_turn_cancelled_locally(app);
                    current_streaming_text.clear();
                    let prompt_restored = app.restore_last_submitted_prompt_if_empty();
                    app.status_message = Some(
                        if prompt_restored {
                            "Request cancelled; prompt restored to composer"
                        } else {
                            "Request cancelled"
                        }
                        .to_string(),
                    );
                    app.disarm_quit();
                }
                CtrlCDisposition::ConfirmExit => {
                    let _ = engine_handle.send(Op::Shutdown).await;
                    return Ok(KeyEventAction::Return);
                }
                CtrlCDisposition::ArmExit => {
                    app.arm_quit();
                }
            }
        }
        KeyCode::Char('d')
            if key.modifiers.contains(KeyModifiers::CONTROL) && app.input.is_empty() =>
        {
            let _ = engine_handle.send(Op::Shutdown).await;
            return Ok(KeyEventAction::Return);
        }
        // Vim composer mode: Esc from Insert/Visual → Normal.
        // This arm runs before the generic Esc handler so Insert mode
        // Esc doesn't accidentally cancel an in-flight request.
        KeyCode::Esc
            if app.composer.vim_enabled
                && app.composer.vim_mode != crate::tui::app::VimMode::Normal =>
        {
            app.vim_enter_normal();
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Esc if app.clear_composer_attachment_selection() => {
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Esc if mention_menu_open => {
            app.mention_menu_hidden = true;
            app.mention_menu_selected = 0;
        }
        KeyCode::Esc => {
            match next_escape_action(app, slash_menu_open) {
                super::EscapeAction::CloseSlashMenu => {
                    // A popup-style action wins over backtrack — clear
                    // any prime so a stale Primed state can't jump us
                    // straight into Selecting on the next Esc.
                    app.backtrack.reset();
                    app.close_slash_menu();
                }
                super::EscapeAction::CancelRequest => {
                    app.backtrack.reset();
                    engine_handle.cancel();
                    mark_active_turn_cancelled_locally(app);
                    current_streaming_text.clear();
                    app.status_message = Some("Request cancelled".to_string());
                }
                super::EscapeAction::DiscardQueuedDraft => {
                    app.backtrack.reset();
                    app.queued_draft = None;
                    app.status_message = Some("Stopped editing queued message".to_string());
                }
                super::EscapeAction::ClearInput => {
                    app.backtrack.reset();
                    app.edit_in_progress = false;
                    app.clear_input_recoverable();
                }
                super::EscapeAction::Noop => {
                    // Nothing else cares about this Esc — route it
                    // through the backtrack state machine. While
                    // streaming or with the live transcript already
                    // open, fall through silently (#133 acceptance:
                    // "during streaming Esc-Esc is a silent no-op").
                    if app.is_loading
                        || app.view_stack.top_kind() == Some(ModalKind::LiveTranscript)
                    {
                        return Ok(KeyEventAction::Handled);
                    }
                    let total = count_user_history_cells(app);
                    match app.backtrack.handle_esc(total) {
                        crate::tui::backtrack::EscEffect::None => {}
                        crate::tui::backtrack::EscEffect::Prime => {
                            app.status_message =
                                Some("Press Esc again to backtrack".to_string());
                            app.needs_redraw = true;
                        }
                        crate::tui::backtrack::EscEffect::Cancel => {
                            app.status_message = Some("Backtrack canceled".to_string());
                            app.needs_redraw = true;
                        }
                        crate::tui::backtrack::EscEffect::OpenOverlay => {
                            open_backtrack_overlay(app);
                        }
                    }
                }
            }
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SUPER) => {
            app.scroll_up(app.viewport.last_transcript_visible.max(3));
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_up(3);
        }
        KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.scroll_up(3);
        }
        KeyCode::Up
            if key.modifiers.is_empty()
                && mention_menu_open
                && app.mention_menu_selected > 0 =>
        {
            app.mention_menu_selected = app.mention_menu_selected.saturating_sub(1);
        }
        KeyCode::Up if key.modifiers.is_empty() && slash_menu_open => {
            select_previous_slash_menu_entry(app, slash_menu_entries.len());
        }
        KeyCode::Up
            if key.modifiers.is_empty()
                && app.selected_composer_attachment_index().is_some() =>
        {
            let _ = app.select_previous_composer_attachment();
        }
        KeyCode::Up
            if key.modifiers.is_empty()
                && app.cursor_position == 0
                && !mention_menu_open
                && !slash_menu_open
                && app.composer_attachment_count() > 0 =>
        {
            let _ = app.select_previous_composer_attachment();
            return Ok(KeyEventAction::Handled);
        }
        // #85: ↑ edits the most-recent queued message when the composer
        // is idle and the pending-input preview is showing queued work.
        KeyCode::Up
            if key.modifiers.is_empty()
                && app.input.is_empty()
                && app.cursor_position == 0
                && app.queued_draft.is_none()
                && !app.queued_messages.is_empty()
                && !mention_menu_open
                && !slash_menu_open
                && app.selected_composer_attachment_index().is_none() =>
        {
            let _ = app.pop_last_queued_into_draft();
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SUPER) => {
            app.scroll_down(app.viewport.last_transcript_visible.max(3));
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::ALT) => {
            app.scroll_down(3);
        }
        KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.scroll_down(3);
        }
        KeyCode::Down if key.modifiers.is_empty() && mention_menu_open => {
            app.mention_menu_selected = (app.mention_menu_selected + 1)
                .min(mention_menu_entries.len().saturating_sub(1));
        }
        KeyCode::Down if key.modifiers.is_empty() && slash_menu_open => {
            select_next_slash_menu_entry(app, slash_menu_entries.len());
        }
        KeyCode::Down
            if key.modifiers.is_empty()
                && app.selected_composer_attachment_index().is_some() =>
        {
            let _ = app.select_next_composer_attachment();
        }
        KeyCode::PageUp => {
            let page = app.viewport.last_transcript_visible.max(1);
            app.scroll_up(page);
        }
        KeyCode::PageDown => {
            let page = app.viewport.last_transcript_visible.max(1);
            app.scroll_down(page);
        }
        KeyCode::Tab => {
            if mention_menu_open
                && crate::tui::file_mention::apply_mention_menu_selection(
                    app,
                    &mention_menu_entries,
                )
            {
                return Ok(KeyEventAction::Handled);
            }
            if slash_menu_open && apply_slash_menu_selection(app, &slash_menu_entries, true) {
                return Ok(KeyEventAction::Handled);
            }
            if try_autocomplete_slash_command(app) {
                return Ok(KeyEventAction::Handled);
            }
            if crate::tui::file_mention::try_autocomplete_file_mention(app) {
                return Ok(KeyEventAction::Handled);
            }
            if app.is_loading && queue_current_draft_for_next_turn(app) {
                return Ok(KeyEventAction::Handled);
            }
            let prior_model = app.model.clone();
            app.cycle_mode();
            if app.model != prior_model {
                let _ = engine_handle
                    .send(Op::SetModel {
                        model: app.model.clone(),
                    })
                    .await;
            }
        }
        KeyCode::BackTab => {
            app.cycle_effort();
        }
        // Transcript-nav shortcuts now require Alt, leaving the bare
        // letters free to insert as text. Before v0.8.30, bare `g`,
        // `G`, `[`, `]`, `?`, `l`, and `v` on an empty composer were
        // hijacked for navigation — typing "good" yielded "ood" with
        // no whale and no warning. The Alt-prefixed shortcuts mirror
        // the Alt+R / Alt+V / Alt+C pattern already in use. Shift is
        // permitted so capital-letter forms (e.g. `Alt+Shift+G` for
        // bottom) work; Ctrl/Super are blocked so the bindings don't
        // collide with platform clipboard / window shortcuts.
        KeyCode::Char('g')
            if crate::tui::key_shortcuts::alt_nav_modifiers(key.modifiers)
                && app.input.is_empty()
                && !slash_menu_open =>
        {
            if let Some(anchor) =
                crate::tui::scrolling::TranscriptScroll::anchor_for(
                    app.viewport.transcript_cache.line_meta(),
                    0,
                )
            {
                app.viewport.transcript_scroll = anchor;
            }
        }
        KeyCode::Char('G')
            if crate::tui::key_shortcuts::alt_nav_modifiers(key.modifiers)
                && app.input.is_empty()
                && !slash_menu_open =>
        {
            app.scroll_to_bottom();
        }
        KeyCode::Char('[')
            if crate::tui::key_shortcuts::alt_nav_modifiers(key.modifiers)
                && app.input.is_empty()
                && !slash_menu_open
                && !jump_to_adjacent_tool_cell(app, SearchDirection::Backward) =>
        {
            app.status_message = Some("No previous tool output".to_string());
        }
        KeyCode::Char(']')
            if crate::tui::key_shortcuts::alt_nav_modifiers(key.modifiers)
                && app.input.is_empty()
                && !slash_menu_open
                && !jump_to_adjacent_tool_cell(app, SearchDirection::Forward) =>
        {
            app.status_message = Some("No next tool output".to_string());
        }
        // `Alt+?` opens the searchable help overlay (#93). F1 and
        // Ctrl+/ are also bound; bare `?` is reserved as text input
        // so users can start a message with "?" without losing the
        // first character.
        KeyCode::Char('?')
            if crate::tui::key_shortcuts::alt_nav_modifiers(key.modifiers)
                && app.input.is_empty()
                && !slash_menu_open =>
        {
            if app.view_stack.top_kind() != Some(ModalKind::Help) {
                app.view_stack.push(HelpView::new_for_locale(app.ui_locale));
            }
            return Ok(KeyEventAction::Handled);
        }
        // Shift+Enter steers a running turn. When idle, the
        // normal composer-newline branch below still handles it
        // as a multiline input gesture.
        KeyCode::Enter
            if app.is_loading
                && key.modifiers.contains(KeyModifiers::SHIFT)
                && !key.modifiers.contains(KeyModifiers::CONTROL)
                && !key.modifiers.contains(KeyModifiers::ALT) =>
        {
            if let Some(input) = app.submit_input() {
                if looks_like_slash_command_input(&input) {
                    if execute_command_input(
                        terminal,
                        app,
                        engine_handle,
                        task_manager,
                        config,
                        web_config_session,
                        &input,
                    )
                    .await?
                    {
                        return Ok(KeyEventAction::Return);
                    }
                } else {
                    let queued = if let Some(mut draft) = app.queued_draft.take() {
                        draft.display = input;
                        draft
                    } else {
                        build_queued_message(app, input)
                    };
                    if let Err(err) =
                        steer_user_message(app, engine_handle, queued.clone()).await
                    {
                        app.queue_message(queued);
                        app.status_message = Some(format!(
                            "Steer failed ({err}); queued {} message(s)",
                            app.queued_message_count()
                        ));
                    }
                }
            }
        }
        // Input handling
        _ if is_composer_newline_key(key) => {
            app.insert_char('\n');
        }
        KeyCode::Enter
            if mention_menu_open
                && crate::tui::file_mention::apply_mention_menu_selection(
                    app,
                    &mention_menu_entries,
                ) =>
        {
            return Ok(KeyEventAction::Handled);
        }
        // #382: Ctrl+Enter forces a steer into the current turn.
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(input) = app.submit_input() {
                if looks_like_slash_command_input(&input) {
                    if execute_command_input(
                        terminal,
                        app,
                        engine_handle,
                        task_manager,
                        config,
                        web_config_session,
                        &input,
                    )
                    .await?
                    {
                        return Ok(KeyEventAction::Return);
                    }
                } else {
                    let queued = if let Some(mut draft) = app.queued_draft.take() {
                        draft.display = input;
                        draft
                    } else {
                        build_queued_message(app, input)
                    };
                    if app.is_loading {
                        // Engine is busy — steer into the current turn.
                        if let Err(err) =
                            steer_user_message(app, engine_handle, queued.clone()).await
                        {
                            app.queue_message(queued);
                            app.status_message = Some(format!(
                                "Steer failed ({err}); queued {} message(s)",
                                app.queued_message_count()
                            ));
                        }
                    } else {
                        // Engine is idle — send as a regular message
                        // so the content is not lost to rx_steer's
                        // stale-drain in handle_send_message (#1331).
                        submit_or_steer_message(app, config, engine_handle, queued).await?;
                    }
                }
            }
        }
        KeyCode::Enter => {
            // #573: when the user typed a slash-command prefix that
            // the popup is matching (e.g. `/mo` → `/model`), Enter
            // should run the *highlighted match* rather than
            // sending the literal `/mo` text. Only kick in when the
            // popup has at least one entry; otherwise fall through
            // to the legacy submit path.
            if slash_menu_open
                && !slash_menu_entries.is_empty()
                && looks_like_slash_command_input(&app.input)
                && apply_slash_menu_selection(app, &slash_menu_entries, false)
            {
                app.close_slash_menu();
            }
            if let Some(input) = app.handle_composer_enter() {
                if handle_plan_choice(app, config, engine_handle, &input).await? {
                    return Ok(KeyEventAction::Handled);
                }
                // `# foo` quick-add (#492) — when memory is enabled,
                // a single line starting with `#` (but not `##` /
                // `#!` shebangs / Markdown headings the user might
                // be pasting in) is intercepted: the text is
                // appended to the user memory file and the input
                // is consumed without firing a turn. Disabled
                // behaviour falls through to normal turn submit.
                if config.memory_enabled() && is_memory_quick_add(&input) {
                    handle_memory_quick_add(app, &input, config);
                    return Ok(KeyEventAction::Handled);
                }
                if looks_like_slash_command_input(&input) {
                    if execute_command_input(
                        terminal,
                        app,
                        engine_handle,
                        task_manager,
                        config,
                        web_config_session,
                        &input,
                    )
                    .await?
                    {
                        return Ok(KeyEventAction::Return);
                    }
                } else {
                    let queued = if let Some(mut draft) = app.queued_draft.take() {
                        draft.display = input;
                        draft
                    } else {
                        build_queued_message(app, input)
                    };
                    // #383: /edit — if the user invoked /edit to revise
                    // the last message, undo the last exchange before
                    // dispatching the replacement. Sync the engine
                    // session so it also drops the old exchange.
                    if app.edit_in_progress {
                        crate::commands::execute("/undo", app);
                        app.edit_in_progress = false;
                        let _ = engine_handle
                            .send(Op::SyncSession {
                                session_id: app.current_session_id.clone(),
                                messages: app.api_messages.clone(),
                                system_prompt: app.system_prompt.clone(),
                                system_prompt_override: false,
                                model: app.model.clone(),
                                workspace: app.workspace.clone(),
                            })
                            .await;
                    }
                    submit_or_steer_message(app, config, engine_handle, queued).await?;
                }
            }
        }
        KeyCode::Backspace
            if key.modifiers.contains(KeyModifiers::SUPER)
                && !app.remove_selected_composer_attachment() =>
        {
            app.delete_to_start_of_line();
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::SUPER) => {}
        KeyCode::Backspace
            if key.modifiers.contains(KeyModifiers::ALT)
                && !app.remove_selected_composer_attachment() =>
        {
            app.delete_word_backward();
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::ALT) => {}
        KeyCode::Backspace
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !app.remove_selected_composer_attachment() =>
        {
            app.delete_word_backward();
        }
        KeyCode::Backspace if key.modifiers.contains(KeyModifiers::CONTROL) => {}
        KeyCode::Delete
            if key.modifiers.contains(KeyModifiers::ALT)
                && !app.remove_selected_composer_attachment() =>
        {
            app.delete_word_forward();
        }
        KeyCode::Delete if key.modifiers.contains(KeyModifiers::ALT) => {}
        KeyCode::Delete
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && !app.remove_selected_composer_attachment() =>
        {
            app.delete_word_forward();
        }
        KeyCode::Delete if key.modifiers.contains(KeyModifiers::CONTROL) => {}
        KeyCode::Backspace if !app.remove_selected_composer_attachment() => {
            app.delete_char();
        }
        KeyCode::Backspace => {}
        KeyCode::Char('h')
            if key_shortcuts_is_ctrl_h_backspace(&key)
                && !app.remove_selected_composer_attachment() =>
        {
            app.delete_char();
        }
        KeyCode::Char('h') if key_shortcuts_is_ctrl_h_backspace(&key) => {}
        KeyCode::Delete if !app.remove_selected_composer_attachment() => {
            app.delete_char_forward();
        }
        KeyCode::Delete => {}
        KeyCode::Left if is_word_cursor_modifier(key.modifiers) => {
            app.move_cursor_word_backward();
        }
        KeyCode::Left => {
            app.move_cursor_left();
        }
        KeyCode::Right if is_word_cursor_modifier(key.modifiers) => {
            app.move_cursor_word_forward();
        }
        KeyCode::Right => {
            app.move_cursor_right();
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
            if let Some(anchor) =
                crate::tui::scrolling::TranscriptScroll::anchor_for(
                    app.viewport.transcript_cache.line_meta(),
                    0,
                )
            {
                app.viewport.transcript_scroll = anchor;
            }
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.scroll_to_bottom();
        }
        KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.move_cursor_start();
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.select_all();
        }
        KeyCode::Home => {
            app.move_cursor_line_start();
        }
        KeyCode::End => {
            app.move_cursor_line_end();
        }
        KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.move_cursor_end();
        }
        KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // Ctrl+O: spawn $EDITOR on the composer contents (#91).
            // Only fires when no modal is active (the !view_stack
            // branch above already returns early in that case) and
            // the composer is the focused input target. We accept the
            // shortcut whether or not a model turn is streaming —
            // editing the buffer never disturbs in-flight work.
            let seed = app.input.clone();
            match crate::tui::external_editor::spawn_editor_for_input(
                terminal,
                app.use_alt_screen,
                app.use_mouse_capture,
                app.use_bracketed_paste,
                &seed,
            ) {
                Ok(crate::tui::external_editor::EditorOutcome::Edited(new)) => {
                    app.input = new;
                    app.move_cursor_end();
                    let editor = std::env::var("VISUAL")
                        .ok()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| {
                            std::env::var("EDITOR")
                                .ok()
                                .filter(|s| !s.trim().is_empty())
                        })
                        .unwrap_or_else(|| "vi".to_string());
                    app.status_message = Some(format!("Edited in {editor}"));
                }
                Ok(crate::tui::external_editor::EditorOutcome::Unchanged) => {
                    app.status_message = Some("Editor closed (no changes)".to_string());
                }
                Ok(crate::tui::external_editor::EditorOutcome::Cancelled) => {
                    app.status_message = Some("Editor cancelled".to_string());
                }
                Err(err) => {
                    app.status_message = Some(format!("Editor error: {err}"));
                }
            }
            app.needs_redraw = true;
        }
        KeyCode::Up => {
            let _ =
                handle_composer_history_arrow(app, key, slash_menu_open, mention_menu_open);
        }
        KeyCode::Down => {
            let _ =
                handle_composer_history_arrow(app, key, slash_menu_open, mention_menu_open);
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.clear_input_recoverable();
        }
        KeyCode::Char('w') | KeyCode::Char('W')
            if key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            app.delete_word_backward();
        }
        KeyCode::Char('s') | KeyCode::Char('S')
            if key.modifiers == KeyModifiers::CONTROL && !app.input.is_empty() =>
        {
            // #440: park the current draft to the persistent
            // stash and clear the composer. Empty composers
            // are a no-op so a stray Ctrl+S can't pollute the
            // file. Surface a toast so the user sees the
            // confirmation (no-op feels broken otherwise).
            crate::composer_stash::push_stash(&app.input);
            app.clear_input_recoverable();
            app.push_status_toast(
                "Draft stashed — `/stash pop` to restore",
                crate::tui::app::StatusToastLevel::Info,
                Some(3_000),
            );
        }
        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            // #379: context-sensitive Ctrl+Y.
            // When the composer has content → emacs-style yank
            // from the kill buffer at the cursor.
            // When the composer is empty (transcript focus) →
            // copy the focused cell text to the system clipboard.
            if app.input.is_empty() && app.view_stack.is_empty() {
                if copy_focused_cell(app) {
                    app.push_status_toast(
                        "Copied to clipboard",
                        crate::tui::app::StatusToastLevel::Info,
                        Some(2_000),
                    );
                } else {
                    app.status_message = Some("No transcript cell to copy".to_string());
                }
            } else {
                app.yank();
            }
        }
        KeyCode::Char('Y') if app.input.is_empty() && app.view_stack.is_empty() => {
            if copy_thinking_cell(app) {
                app.push_status_toast(
                    "Thinking copied to clipboard",
                    crate::tui::app::StatusToastLevel::Info,
                    Some(2_000),
                );
            }
        }
        KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let new_mode = match app.mode {
                AppMode::Plan => AppMode::Agent,
                _ => AppMode::Plan,
            };
            app.set_mode(new_mode);
        }
        _ if key_shortcuts_is_paste_shortcut(&key) => {
            app.paste_from_clipboard();
        }
        KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_mode(AppMode::Agent);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_mode(AppMode::Yolo);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_mode(AppMode::Plan);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('A') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_mode(AppMode::Agent);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('Y') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_mode(AppMode::Yolo);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('P') if key.modifiers.contains(KeyModifiers::ALT) => {
            app.set_mode(AppMode::Plan);
            return Ok(KeyEventAction::Handled);
        }
        KeyCode::Char('v') | KeyCode::Char('V')
            if key.modifiers.contains(KeyModifiers::ALT) =>
        {
            open_tool_details_pager(app);
            return Ok(KeyEventAction::Handled);
        }
        // Vim composer: Normal-mode motion / operator keys.
        // Only fires when vim is enabled, the input is focused (no modal
        // open on top), and the key has no modifier (pure char).
        KeyCode::Char(c)
            if app.vim_is_normal_mode()
                && key.modifiers.is_empty()
                && !slash_menu_open
                && !mention_menu_open
                && app.view_stack.is_empty() =>
        {
            crate::tui::vim_mode::handle_vim_normal_key(app, c);
            return Ok(KeyEventAction::Handled);
        }
        // Vim composer: in Visual mode plain chars are ignored
        // (no text insertion until `i` / `a` enters Insert).
        KeyCode::Char(_)
            if app.vim_is_visual_mode()
                && key.modifiers.is_empty()
                && app.view_stack.is_empty() =>
        {
            // absorb — Visual mode not yet fully implemented
        }
        KeyCode::Char(c) => {
            app.insert_char(c);
        }
        _ => {}
    }

    if !is_plain_char && !is_enter {
        app.paste_burst.clear_window_after_non_char();
    }
    Ok(KeyEventAction::Continue)
}

// Re-exports kept local so the extracted module compiles standalone from
// the ui super-module without dragging in the whole key_shortcuts surface.
fn key_shortcuts_is_ctrl_h_backspace(key: &KeyEvent) -> bool {
    crate::tui::key_shortcuts::is_ctrl_h_backspace(key)
}
fn key_shortcuts_is_paste_shortcut(key: &KeyEvent) -> bool {
    crate::tui::key_shortcuts::is_paste_shortcut(key)
}
fn key_shortcuts_is_text_input_key(key: &KeyEvent) -> bool {
    crate::tui::key_shortcuts::is_text_input_key(key)
}
fn key_shortcuts_is_file_tree_toggle_shortcut(key: &KeyEvent) -> bool {
    crate::tui::key_shortcuts::is_file_tree_toggle_shortcut(key)
}
fn key_shortcuts_is_macos_option_v_legacy_key(key: &KeyEvent) -> bool {
    crate::tui::key_shortcuts::is_macos_option_v_legacy_key(key)
}

