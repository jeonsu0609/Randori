//! The terminal at the bottom of a tab, as in GitKraken: the toolbar's
//! terminal button or ⌃` shows and hides it. The shell starts in the
//! workspace the first time the panel opens and keeps running while it is
//! hidden. When a command changes the repository (jj recorded an operation),
//! the tab reloads.

use std::path::Path;

use gpui_kit::component::{WindowExt as _, button::ButtonVariant, notification::Notification};

use crate::terminal::{TerminalEvent, TerminalView};

use super::*;

impl RepoView {
    /// ⌃`: shows the terminal (starting its shell the first time) and focuses
    /// it, or hides it and goes back to the revisions.
    pub(super) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.terminal_open {
            self.terminal_open = false;
            self.log_focus.focus(window, cx);
            cx.notify();
            return;
        }
        let Some(root) = self.workspace.as_ref().map(|ws| ws.root.clone()) else {
            return;
        };
        // Settings may send it to a terminal application instead
        if let Some(app) = settings::get(cx).display.external_terminal.clone() {
            // off the UI thread: `open` waits while the application starts
            cx.spawn_in(window, async move |this, cx| {
                let name = app.clone();
                let result = cx
                    .background_spawn(async move { open_external_terminal(&name, &root) })
                    .await;
                if let Err(err) = result {
                    let _ = this.update_in(cx, |_, window, cx| {
                        window.push_notification(
                            Notification::error(format!("{err:#}"))
                                .title(format!("Could not open {app}")),
                            cx,
                        )
                    });
                }
            })
            .detach();
            return;
        }
        let terminal = match &self.terminal {
            Some(terminal) => terminal.clone(),
            None => {
                let terminal =
                    cx.new(|cx| TerminalView::new(root.clone(), Some(&root), window, cx));
                let subscription = cx.subscribe(&terminal, |view, _, event, cx| match event {
                    TerminalEvent::RepositoryChanged { snapshot } => {
                        view.reload_from_terminal(*snapshot, cx)
                    }
                    // the header reads the title again when the panel opens
                    TerminalEvent::TitleChanged if view.terminal_open => cx.notify(),
                    TerminalEvent::TitleChanged => {}
                });
                self._subscriptions.push(subscription);
                self.terminal = Some(terminal.clone());
                terminal
            }
        };
        self.terminal_open = true;
        terminal.read(cx).focus_handle().clone().focus(window, cx);
        cx.notify();
    }

    /// A command in the terminal changed the repository. After jj, which
    /// snapshotted the working copy itself, only its operation is loaded, no
    /// second scan; after `git` alone, a snapshot imports what Git moved.
    fn reload_from_terminal(&mut self, snapshot: bool, cx: &mut Context<Self>) {
        if self.busy.is_empty() {
            self.refresh_from_disk(snapshot, cx);
        } else {
            // after Randori's own command, with the terminal's change too
            let earlier = self.refresh_after_ops.unwrap_or(false);
            self.refresh_after_ops = Some(earlier || snapshot);
        }
    }

    /// Tells the terminal the tab has caught up with the repository, so its
    /// next output does not reload again for an operation already shown.
    pub(super) fn terminal_caught_up(&self, cx: &mut Context<Self>) {
        if let Some(terminal) = &self.terminal {
            terminal.update(cx, |terminal, cx| terminal.note_repository_seen(cx));
        }
    }

    /// The panel's restart button: a shell that still runs asks first, since
    /// restarting ends whatever runs in it.
    fn restart_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(terminal) = self.terminal.clone() else {
            return;
        };
        if !terminal.read(cx).is_running() {
            restart(&terminal, window, cx);
            return;
        }
        window.open_alert_dialog(cx, move |alert, _, _| {
            let terminal = terminal.clone();
            alert
                .title("Restart the shell?")
                .description("Whatever runs in it now is stopped.")
                .confirm()
                .ok_text("Restart")
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    restart(&terminal, window, cx);
                    true
                })
        });
    }

    /// The body with the terminal under it, when the terminal is open.
    pub(super) fn with_terminal(
        &self,
        body: AnyElement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(terminal) = self.terminal.clone().filter(|_| self.terminal_open) else {
            return body;
        };
        let rem = window.rem_size();
        v_resizable("terminal-split")
            .child(
                resizable_panel()
                    .size_range(rem * 12. ..rem * 400.)
                    .child(body),
            )
            .child(
                resizable_panel()
                    .size(rem * 16.)
                    .size_range(rem * 6. ..rem * 60.)
                    .child(self.render_terminal_panel(terminal, cx)),
            )
            .into_any_element()
    }

    fn render_terminal_panel(
        &self,
        terminal: Entity<TerminalView>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (title, exited) = {
            let terminal = terminal.read(cx);
            (terminal.title(), terminal.has_exited())
        };
        v_flex()
            .size_full()
            .min_h_0()
            .border_t_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .h_8()
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .bg(theme.list_head)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(title),
                    )
                    .child(
                        Button::new("terminal-restart")
                            .ghost()
                            .xsmall()
                            .icon(IconName::RotateCcw)
                            .tooltip(if exited {
                                "Start a new shell"
                            } else {
                                "Restart the shell"
                            })
                            .on_click(
                                cx.listener(|view, _, window, cx| {
                                    view.restart_terminal(window, cx)
                                }),
                            ),
                    )
                    .child(
                        Button::new("terminal-hide")
                            .ghost()
                            .xsmall()
                            .icon(IconName::X)
                            .tooltip("Hide the terminal (⌃`)")
                            .on_click(
                                cx.listener(|view, _, window, cx| view.toggle_terminal(window, cx)),
                            ),
                    ),
            )
            // cached: the tab's own redraws leave the grid alone, and output
            // redraws the grid only
            .child(
                div().flex_1().min_h_0().child(
                    gpui_kit::AnyView::from(terminal)
                        .cached(gpui_kit::StyleRefinement::default().size_full()),
                ),
            )
            .into_any_element()
    }
}

/// Opens terminal application `app` in `dir` (macOS: `open -a`, which
/// starts the application or brings it forward with a new window there).
fn open_external_terminal(app: &str, dir: &Path) -> anyhow::Result<()> {
    if !cfg!(target_os = "macos") {
        anyhow::bail!("Opening another terminal application works on macOS only.");
    }
    let status = std::process::Command::new("open")
        .arg("-a")
        .arg(app)
        .arg(dir)
        .status()?;
    anyhow::ensure!(status.success(), "open -a {app} failed ({status})");
    Ok(())
}

fn restart(terminal: &Entity<TerminalView>, window: &mut Window, cx: &mut App) {
    terminal.update(cx, |terminal, cx| terminal.start(cx));
    terminal.read(cx).focus_handle().clone().focus(window, cx);
}
