//! Terminal-event handling (paste / focus / resize / mouse) for the TUI
//! event loop.

use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event};
use ratatui::layout::{Rect, Size};

use crate::config::Config;
use crate::config_ui::WebConfigSession;
use crate::core::engine::EngineHandle;
use crate::task_manager::SharedTaskManager;
use crate::tui::app::{App, OnboardingState};
use crate::tui::mouse_ui::{handle_mouse_event, should_drop_loading_mouse_motion};
use crate::tui::onboarding;

use super::{
    AppTerminal, draw_app_frame_inner, handle_view_events, recover_terminal_modes,
    terminal_event_needs_viewport_recapture,
};

/// Outcome of handling one non-key terminal event.
pub(super) enum TerminalEventOutcome {
    /// Event fully consumed; the main loop should `continue` to the next poll.
    Consumed,
    /// A view event requested termination; the main loop should return.
    Return,
    /// The event was not a paste/focus/resize/mouse event; hand it back so the
    /// main loop can attempt keyboard handling.
    NotConsumed(Event),
}

/// FocusGained debounce: some terminal emulators (e.g. Tabby) re-trigger
/// FocusGained when we re-arm focus-change reporting inside
/// recover_terminal_modes, creating a tight repaint loop. Skip
/// mode recovery (but still mark a repaint) within the debounce window.
const FOCUS_RECOVERY_DEBOUNCE: Duration = Duration::from_millis(200);

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_terminal_event(
    evt: Event,
    terminal: &mut AppTerminal,
    app: &mut App,
    config: &mut Config,
    engine_handle: &mut EngineHandle,
    task_manager: &SharedTaskManager,
    web_config_session: &mut Option<WebConfigSession>,
    last_focus_recovery: &mut Instant,
    force_terminal_repaint: &mut bool,
    draws_since_last_full_repaint: &mut u64,
) -> Result<TerminalEventOutcome> {
    // Handle bracketed paste events
    if let Event::Paste(text) = &evt {
        tracing::debug!(
            paste_len = text.len(),
            preview = %text.chars().take(80).collect::<String>(),
            "Received bracketed paste event"
        );
        // Once a real bracketed-paste event has been observed in
        // this session, the rapid-keystroke heuristic in
        // paste_burst is redundant — disable it so fast typing /
        // IME commits / autocomplete bursts don't get
        // mis-classified as a paste.
        app.bracketed_paste_seen = true;
        if app.onboarding == OnboardingState::ApiKey {
            // Paste into API key input
            app.insert_api_key_str(text);
            onboarding::sync_api_key_validation_status(app, false);
        } else if app.is_history_search_active() {
            app.history_search_insert_str(text);
        } else if app.view_stack.handle_paste(text) {
            // Modal consumed the paste (e.g. provider picker key entry)
        } else if !app.view_stack.is_empty() {
            // A non-consumed modal is open — don't leak paste into composer
        } else {
            // Paste into main input
            app.insert_paste_text(text);
        }
        return Ok(TerminalEventOutcome::Consumed);
    }

    // Re-establish terminal mode flags on focus-gain and force a full
    // viewport reset before repainting. App-switching and interactive
    // handoffs can leave the host terminal scrolled away from row 0
    // and (on macOS) can drop the keyboard, mouse-tracking, or
    // bracketed-paste modes — recover_terminal_modes() is the
    // canonical place those flags live.
    if terminal_event_needs_viewport_recapture(&evt) {
        let now = Instant::now();
        if now.duration_since(*last_focus_recovery) >= FOCUS_RECOVERY_DEBOUNCE {
            recover_terminal_modes(
                terminal.backend_mut(),
                app.use_mouse_capture,
                app.use_bracketed_paste,
            );
            *last_focus_recovery = now;
        }
        *force_terminal_repaint = true;
        app.needs_redraw = true;
    }

    if let Event::Resize(width, height) = evt {
        tracing::debug!(
            width,
            height,
            coherence = ?app.coherence_state,
            use_alt_screen = app.use_alt_screen,
            "Event::Resize received; clearing terminal"
        );
        // Drain any further Resize events queued in this poll cycle so we
        // act on the final size only, then issue a single clear + redraw.
        // crossterm coalesces some resize events but rapid drag-resizes
        // can still queue several; processing them all here avoids the
        // common "stale art on the right edge" symptom (#65) caused by
        // the diff renderer skipping cells that match a stale back
        // buffer between intermediate sizes.
        let mut final_w = width;
        let mut final_h = height;
        while event::poll(Duration::from_millis(0)).unwrap_or(false) {
            match event::read() {
                Ok(Event::Resize(w, h)) => {
                    final_w = w;
                    final_h = h;
                }
                Ok(other) => {
                    // Non-resize event during the drain: we can't
                    // un-read it. Drop it and let the user re-issue
                    // — the resize-coalesce window is tiny.
                    tracing::debug!(
                        ?other,
                        "non-resize event during resize coalesce; dropping"
                    );
                    break;
                }
                Err(_) => break,
            }
        }

        // #582: commit the event-reported size to ratatui's
        // viewport explicitly before the redraw, instead of
        // relying on `crossterm::terminal::size()` which gets
        // queried internally during `terminal.draw`. On
        // Windows ConHost specifically, `terminal::size()` has
        // been observed to return stale dimensions briefly
        // during a maximize→windowed transition; the next
        // `draw` then paints into a buffer that does not
        // match the post-restore viewport, producing the
        // unrecoverable black screen reported by @imakid.
        // The `Event::Resize` payload itself carries the
        // authoritative new size, so we forward it.
        if let Err(err) = terminal.resize(Rect::new(0, 0, final_w, final_h)) {
            tracing::warn!(
                ?err,
                final_w,
                final_h,
                "terminal.resize during Resize event failed; falling back to clear+draw"
            );
        }

        app.handle_resize(final_w, final_h);
        // #macos-resize: some terminals (macOS Terminal.app, Windows
        // ConHost) briefly report stale dimensions via
        // `terminal::size()` after a resize. ratatui's `draw()` calls
        // `autoresize()` internally, which queries the backend size;
        // if it sees the old dimension it shrinks the viewport back,
        // leaving the newly-expanded area filled with stale content
        // from the previous frame (duplicate UI panels).
        //
        // We force the backend to report the resize-event size for
        // this single draw so the buffer matches the real viewport.
        {
            let backend = terminal.backend_mut();
            backend.force_size(Size::new(final_w, final_h));
        }
        draw_app_frame_inner(terminal, app, true)?;
        *draws_since_last_full_repaint = 0;
        {
            let backend = terminal.backend_mut();
            backend.clear_forced_size();
        }
        app.needs_redraw = false;
        return Ok(TerminalEventOutcome::Consumed);
    }

    if app.use_mouse_capture
        && let Event::Mouse(mouse) = evt
    {
        if should_drop_loading_mouse_motion(app, mouse) {
            return Ok(TerminalEventOutcome::Consumed);
        }
        let events = handle_mouse_event(app, mouse);
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
            return Ok(TerminalEventOutcome::Return);
        }
        return Ok(TerminalEventOutcome::Consumed);
    }

    Ok(TerminalEventOutcome::NotConsumed(evt))
}
