//! A terminal: the user's shell in a pseudo-terminal, drawn as a grid of
//! cells. `alacritty_terminal` runs the shell and keeps the screen (escape
//! sequences, scrollback, selection); this module draws it with GPUI and turns
//! keys, text input, the mouse and the clipboard into what the shell reads.
//!
//! A repository tab shows one at the bottom (`repo_view/terminal_panel.rs`).
//! After output, the view looks at the repository's operation heads and says
//! when they moved, so the tab reloads after a jj command.

mod colors;
mod keys;

use std::borrow::Cow;
use std::cell::Cell as StdCell;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::tty;
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor};
use futures::StreamExt as _;
use futures::channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, AppContext as _, BorderStyle, Bounds, ClipboardItem, Context, ElementInputHandler,
    EntityInputHandler, EventEmitter, FocusHandle, Font, FontStyle, FontWeight, Hsla,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, Pixels, Render, ScrollEvent, SharedString,
    StatefulInteractiveElement as _, StrikethroughStyle, Styled as _, Task, TextAlign, TextRun,
    UTF16Selection, UnderlineStyle, Window, canvas, div, fill, outline, point, px, size,
};

/// Key context of a terminal; its bindings win over the tab's and the app's.
pub const TERMINAL_CONTEXT: &str = "RandoriTerminal";

/// Keyboard actions of a terminal.
pub mod actions {
    gpui_kit::actions!(randori_terminal, [Copy, Paste, Clear]);

    /// Sends a keystroke (written as in a key binding) to the shell. Bound in
    /// the terminal for keys the app would otherwise take, such as Esc.
    #[derive(gpui_kit::Action, Clone, PartialEq)]
    #[action(namespace = randori_terminal, no_json)]
    pub struct SendKey(pub &'static str);
}

/// Keys the tab, the app or gpui-kit's root bind that the shell needs while
/// it has focus: the root moves focus on Tab and copies on Ctrl+C (off macOS).
#[cfg(target_os = "macos")]
const SHELL_KEYS: &[&str] = &["escape", "tab", "shift-tab"];
#[cfg(not(target_os = "macos"))]
const SHELL_KEYS: &[&str] = &[
    "escape",
    "tab",
    "shift-tab",
    "ctrl-b",
    "ctrl-c",
    "ctrl-l",
    "ctrl-o",
    "ctrl-p",
    "ctrl-q",
    "ctrl-r",
    "ctrl-w",
    "ctrl-z",
];

/// Binds the terminal's keys. Call once at startup, after the app's.
pub fn bind_keys(cx: &mut App) {
    use gpui_kit::KeyBinding;
    let ctx = Some(TERMINAL_CONTEXT);
    let mut bindings: Vec<KeyBinding> = SHELL_KEYS
        .iter()
        .map(|key| KeyBinding::new(key, actions::SendKey(key), ctx))
        .collect();
    if cfg!(target_os = "macos") {
        bindings.extend([
            KeyBinding::new("cmd-c", actions::Copy, ctx),
            KeyBinding::new("cmd-v", actions::Paste, ctx),
            KeyBinding::new("cmd-k", actions::Clear, ctx),
        ]);
    } else {
        bindings.extend([
            KeyBinding::new("ctrl-shift-c", actions::Copy, ctx),
            KeyBinding::new("ctrl-shift-v", actions::Paste, ctx),
        ]);
    }
    cx.bind_keys(bindings);
}

/// What the terminal tells its tab.
pub enum TerminalEvent {
    /// A command changed the repository. Without `snapshot`, jj ran and
    /// recorded an operation; with it, only the colocated Git repository
    /// moved (`git commit`, `git checkout`), which jj imports on a snapshot.
    RepositoryChanged { snapshot: bool },
    /// The shell's title changed (the running command or directory).
    TitleChanged,
}

/// Columns and rows of the screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridSize {
    columns: usize,
    lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

/// Where the grid was painted, for turning pointer positions into cells.
#[derive(Clone, Copy, Debug, Default)]
struct Geometry {
    origin: Point<Pixels>,
    cell: gpui_kit::Size<Pixels>,
    columns: usize,
    lines: usize,
    /// The cursor cell, for placing an input method's candidate window.
    cursor: Option<Bounds<Pixels>>,
}

use gpui_kit::Point;

/// Forwards the terminal's events to the view.
#[derive(Clone)]
struct Listener {
    events: UnboundedSender<Event>,
    /// Set while a wakeup waits for the view. The PTY thread sends one per
    /// read (up to 64 KB), and the next frame shows all of them at once, so
    /// fast output queues one wakeup instead of thousands.
    wakeup_pending: Arc<AtomicBool>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if matches!(event, Event::Wakeup) && self.wakeup_pending.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = self.events.unbounded_send(event);
    }
}

/// The running shell: its screen and the channel to its PTY thread.
struct Shell {
    term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
}

impl Drop for Shell {
    fn drop(&mut self) {
        // ends the PTY thread, which hangs up on the shell
        let _ = self.sender.send(Msg::Shutdown);
    }
}

pub struct TerminalView {
    shell: Option<Shell>,
    /// Why the shell could not start, shown instead of the grid.
    error: Option<SharedString>,
    exited: bool,
    title: SharedString,
    cwd: PathBuf,
    /// What tells that a command changed the repository, read after output.
    watch: Option<RepoWatch>,
    seen: Option<RepoSignature>,
    check_pending: bool,
    focus: FocusHandle,
    size: GridSize,
    geometry: Rc<StdCell<Geometry>>,
    /// Text an input method is composing, drawn at the cursor.
    marked: Option<String>,
    /// A pointer press that may become a selection.
    selecting: bool,
    /// Wheel travel not yet turned into whole lines.
    scroll_remainder: Pixels,
    _events: Option<Task<()>>,
    _focus_subscriptions: Vec<gpui_kit::Subscription>,
    /// The terminal font of Settings the grid is drawn with, its family
    /// only when installed (`None`: the code font's).
    font_setting: (Option<SharedString>, Option<f32>),
}

impl EventEmitter<TerminalEvent> for TerminalView {}

impl TerminalView {
    /// A shell in `cwd`; `workspace_root` is the jj workspace whose operation
    /// log is watched.
    pub fn new(
        cwd: PathBuf,
        workspace_root: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        // the tab draws this view cached: redraw for the cursor's focus look
        let focus_subscriptions = vec![
            cx.on_focus_in(&focus, window, |_, _, cx| cx.notify()),
            cx.on_focus_out(&focus, window, |_, _, _, cx| cx.notify()),
            // and for a theme change, which recolours the grid
            cx.observe_global::<gpui_kit::component::Theme>(|_, cx| cx.notify()),
            // and for the terminal font of Settings, not its other changes
            cx.observe_global::<crate::settings::Settings>(|view: &mut Self, cx| {
                view.follow_font_setting(cx)
            }),
            // and once for the font listing: it brings the symbol fonts the
            // grid falls back on, and may find the chosen font only now
            cx.observe_global::<crate::settings_window::CodeFonts>(|view: &mut Self, cx| {
                view.font_setting = terminal_font_setting(cx);
                cx.notify();
            }),
        ];
        let watch = workspace_root.and_then(RepoWatch::of);
        let mut view = Self {
            shell: None,
            error: None,
            exited: false,
            title: "Terminal".into(),
            cwd,
            watch,
            // read in the background below: walking `.git/refs` of a repository
            // with many remote branches takes a while
            seen: None,
            check_pending: false,
            focus,
            _focus_subscriptions: focus_subscriptions,
            size: GridSize {
                columns: 80,
                lines: 24,
            },
            geometry: Rc::default(),
            marked: None,
            selecting: false,
            scroll_remainder: px(0.),
            _events: None,
            font_setting: terminal_font_setting(cx),
        };
        view.note_repository_seen(cx);
        view.start(cx);
        view
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// A shell that still runs: restarting it would end what runs in it.
    pub fn is_running(&self) -> bool {
        self.shell.is_some() && !self.has_exited()
    }

    pub fn has_exited(&self) -> bool {
        self.exited || self.error.is_some()
    }

    /// Redraws for a new terminal font, and only then.
    fn follow_font_setting(&mut self, cx: &mut Context<Self>) {
        let font = terminal_font_setting(cx);
        if font != self.font_setting {
            self.font_setting = font;
            cx.notify();
        }
    }

    /// Starts (or restarts) the shell.
    pub fn start(&mut self, cx: &mut Context<Self>) {
        self.shell = None;
        self.exited = false;
        self.error = None;
        let (tx, mut rx) = unbounded();
        let wakeup_pending = Arc::new(AtomicBool::new(false));
        let listener = Listener {
            events: tx,
            wakeup_pending: wakeup_pending.clone(),
        };
        match spawn_shell(&self.cwd, self.size, listener) {
            Ok(shell) => self.shell = Some(shell),
            Err(err) => {
                log::warn!("starting the terminal's shell: {err:#}");
                self.error = Some(format!("Couldn't start a shell: {err}").into());
            }
        }
        self._events = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = rx.next().await {
                if matches!(event, Event::Wakeup) {
                    // before the frame reads the screen: output after this
                    // point sends a wakeup of its own
                    wakeup_pending.store(false, Ordering::Release);
                }
                if this
                    .update(cx, |view, cx| view.handle_event(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Wakeup => {
                self.schedule_repository_check(cx);
                cx.notify();
            }
            Event::PtyWrite(text) => self.write(text.into_bytes()),
            Event::Title(title) => self.set_title(title.into(), cx),
            Event::ResetTitle => self.set_title("Terminal".into(), cx),
            Event::ClipboardStore(_, text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            Event::ClipboardLoad(_, format) => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.write(format(&text).into_bytes());
                }
            }
            Event::ColorRequest(ix, format) => {
                let rgb = self.color_for_request(ix, cx);
                self.write(format(rgb).into_bytes());
            }
            Event::TextAreaSizeRequest(format) => {
                let size = self.window_size();
                self.write(format(size).into_bytes());
            }
            Event::ChildExit(_) | Event::Exit => {
                self.exited = true;
                // the last command may have changed the repository
                self.schedule_repository_check(cx);
                cx.notify();
            }
            Event::Bell | Event::CursorBlinkingChange | Event::MouseCursorDirty => {}
        }
    }

    /// The panel's header shows the title, not the grid: so the tab is told,
    /// and only when the title changed (shells set it at every prompt).
    fn set_title(&mut self, title: SharedString, cx: &mut Context<Self>) {
        if self.title != title {
            self.title = title;
            cx.emit(TerminalEvent::TitleChanged);
        }
    }

    fn color_for_request(&self, ix: usize, cx: &App) -> alacritty_terminal::vte::ansi::Rgb {
        let theme = cx.theme();
        let to_rgb = |color: Hsla| {
            let rgba = color.to_rgb();
            alacritty_terminal::vte::ansi::Rgb {
                r: (rgba.r * 255.) as u8,
                g: (rgba.g * 255.) as u8,
                b: (rgba.b * 255.) as u8,
            }
        };
        if let Some(shell) = &self.shell
            && let Some(rgb) = shell.term.lock().colors()[ix]
        {
            return rgb;
        }
        match ix {
            ix if ix == NamedColor::Foreground as usize => to_rgb(theme.foreground),
            ix if ix == NamedColor::Background as usize => to_rgb(theme.background),
            ix if ix < 256 => colors::default_indexed(ix, theme.mode.is_dark()),
            _ => to_rgb(theme.foreground),
        }
    }

    fn window_size(&self) -> WindowSize {
        let cell = self.geometry.get().cell;
        WindowSize {
            num_lines: self.size.lines as u16,
            num_cols: self.size.columns as u16,
            cell_width: f32::from(cell.width) as u16,
            cell_height: f32::from(cell.height) as u16,
        }
    }

    /// Sends bytes to the shell, as typed: the view follows to the bottom and
    /// a selection is dropped.
    fn type_bytes(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if let Some(shell) = &self.shell {
            let mut term = shell.term.lock();
            term.scroll_display(Scroll::Bottom);
            term.selection = None;
        }
        self.write(bytes);
        cx.notify();
    }

    fn write(&self, bytes: Vec<u8>) {
        if bytes.is_empty() {
            return;
        }
        if let Some(shell) = &self.shell {
            let _ = shell.sender.send(Msg::Input(Cow::Owned(bytes)));
        }
    }

    fn mode(&self) -> TermMode {
        self.shell
            .as_ref()
            .map_or(TermMode::empty(), |shell| *shell.term.lock().mode())
    }

    /// Called from paint with the grid that fits; resizes the screen and the
    /// PTY when it changed.
    fn set_size(&mut self, size: GridSize, cell: gpui_kit::Size<Pixels>) {
        if size == self.size || size.columns == 0 || size.lines == 0 {
            return;
        }
        self.size = size;
        if let Some(shell) = &self.shell {
            shell.term.lock().resize(size);
            let _ = shell.sender.send(Msg::Resize(WindowSize {
                num_lines: size.lines as u16,
                num_cols: size.columns as u16,
                cell_width: f32::from(cell.width) as u16,
                cell_height: f32::from(cell.height) as u16,
            }));
        }
    }

    // ----- the repository -------------------------------------------------------------------

    /// Output may be a command finishing; look at the repository a moment
    /// later, once the command has written it.
    fn schedule_repository_check(&mut self, cx: &mut Context<Self>) {
        let Some(watch) = self.watch.clone().filter(|_| !self.check_pending) else {
            return;
        };
        self.check_pending = true;
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(400))
                .await;
            let now = cx.background_spawn(async move { watch.read() }).await;
            let _ = this.update(cx, |view, cx| {
                view.check_pending = false;
                let change = repository_change(view.seen.as_ref(), &now);
                view.seen = Some(now);
                if let Some(change) = change {
                    cx.emit(change);
                }
            });
        })
        .detach();
    }

    /// Takes the repository as it is now as seen: when the terminal starts,
    /// and when the tab reloaded for its own reasons (its own commands), so
    /// the next output does not reload again.
    pub fn note_repository_seen(&mut self, cx: &mut Context<Self>) {
        let Some(watch) = self.watch.clone() else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let now = cx.background_spawn(async move { watch.read() }).await;
            let _ = this.update(cx, |view, _| view.seen = Some(now));
        })
        .detach();
    }

    // ----- input ----------------------------------------------------------------------------

    fn key_down(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.has_exited() {
            if event.keystroke.key == "enter" {
                self.start(cx);
                cx.stop_propagation();
            }
            return;
        }
        // an input method is composing: its keys are its own
        if self.marked.is_some() {
            return;
        }
        if let Some(bytes) = keys::bytes_for(&event.keystroke, self.mode()) {
            self.type_bytes(bytes, cx);
            cx.stop_propagation();
        }
    }

    fn send_key(&mut self, action: &actions::SendKey, _: &mut Window, cx: &mut Context<Self>) {
        let Ok(keystroke) = gpui_kit::Keystroke::parse(action.0) else {
            return;
        };
        if self.has_exited() || self.marked.is_some() {
            cx.propagate();
            return;
        }
        if let Some(bytes) = keys::bytes_for(&keystroke, self.mode()) {
            self.type_bytes(bytes, cx);
        }
    }

    fn copy(&mut self, _: &actions::Copy, _: &mut Window, cx: &mut Context<Self>) {
        let text = self
            .shell
            .as_ref()
            .and_then(|shell| shell.term.lock().selection_to_string());
        match text {
            Some(text) if !text.is_empty() => {
                cx.write_to_clipboard(ClipboardItem::new_string(text))
            }
            // nothing selected: ⌘C is not the shell's interrupt, so do nothing
            _ => {}
        }
    }

    fn paste(&mut self, _: &actions::Paste, _: &mut Window, cx: &mut Context<Self>) {
        if self.has_exited() {
            return;
        }
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let bytes = keys::paste_bytes(&text, self.mode());
            self.type_bytes(bytes, cx);
        }
    }

    /// ⌘K: clears the screen and the scrollback, as macOS Terminal does; the
    /// shell redraws its prompt after the form feed.
    fn clear(&mut self, _: &actions::Clear, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(shell) = &self.shell {
            let mut term = shell.term.lock();
            term.grid_mut().clear_history();
            term.selection = None;
        }
        self.type_bytes(vec![0x0c], cx);
    }

    fn grid_point(&self, position: Point<Pixels>) -> (GridPoint, Side) {
        let geometry = self.geometry.get();
        let offset = self
            .shell
            .as_ref()
            .map_or(0, |shell| shell.term.lock().grid().display_offset());
        let x = f32::from(position.x - geometry.origin.x);
        let y = f32::from(position.y - geometry.origin.y);
        let width = f32::from(geometry.cell.width).max(1.);
        let height = f32::from(geometry.cell.height).max(1.);
        let column = (x / width)
            .floor()
            .clamp(0., geometry.columns.saturating_sub(1) as f32);
        let line = (y / height)
            .floor()
            .clamp(0., geometry.lines.saturating_sub(1) as f32);
        let side = if x / width - column > 0.5 {
            Side::Right
        } else {
            Side::Left
        };
        let point = GridPoint::new(Line(line as i32 - offset as i32), Column(column as usize));
        (point, side)
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.focus.focus(window, cx);
        let (point, side) = self.grid_point(event.position);
        let Some(shell) = &self.shell else {
            return;
        };
        let mut term = shell.term.lock();
        let ty = match event.click_count {
            2 => SelectionType::Semantic,
            3.. => SelectionType::Lines,
            _ => SelectionType::Simple,
        };
        if event.modifiers.shift
            && let Some(selection) = term.selection.as_mut()
        {
            selection.update(point, side);
        } else {
            term.selection = Some(Selection::new(ty, point, side));
        }
        drop(term);
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (point, side) = self.grid_point(event.position);
        if let Some(shell) = &self.shell
            && let Some(selection) = shell.term.lock().selection.as_mut()
        {
            selection.update(point, side);
        }
        cx.notify();
    }

    fn mouse_up(&mut self, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        // a click without a drag leaves nothing selected
        if let Some(shell) = &self.shell {
            let mut term = shell.term.lock();
            if term
                .selection
                .as_ref()
                .is_some_and(|selection| selection.is_empty())
            {
                term.selection = None;
            }
        }
        cx.notify();
    }

    fn scroll(&mut self, event: &ScrollEvent, _: &mut Window, cx: &mut Context<Self>) {
        let cell_height = self.geometry.get().cell.height.max(px(1.));
        let delta = event.delta.pixel_delta(cell_height).y + self.scroll_remainder;
        let lines = (f32::from(delta) / f32::from(cell_height)).trunc() as i32;
        self.scroll_remainder = delta - cell_height * lines as f32;
        if lines == 0 {
            return;
        }
        let mode = self.mode();
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            // full-screen programs (less, vim) scroll by their own arrows
            let arrow = if lines > 0 { 'A' } else { 'B' };
            let prefix = if mode.contains(TermMode::APP_CURSOR) {
                "\x1bO"
            } else {
                "\x1b["
            };
            let bytes = format!("{prefix}{arrow}").repeat(lines.unsigned_abs() as usize);
            self.write(bytes.into_bytes());
        } else if let Some(shell) = &self.shell {
            shell.term.lock().scroll_display(Scroll::Delta(lines));
        }
        cx.notify();
    }
}

impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        _: Range<usize>,
        _: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        None
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: 0..0,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|text| 0..text.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = None;
        if self.has_exited() {
            cx.notify();
            return;
        }
        self.type_bytes(text.as_bytes().to_vec(), cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _: Option<Range<usize>>,
        new_text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.marked = (!new_text.is_empty()).then(|| new_text.to_owned());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        self.geometry.get().cursor
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }

    fn accepts_text_input(&self, _: &mut Window, _: &mut Context<Self>) -> bool {
        !self.has_exited()
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let background = theme.background;
        // Settings' terminal font, else the code font
        let (family, size) = &self.font_setting;
        let family = family
            .clone()
            .unwrap_or_else(|| theme.mono_font_family.clone());
        let mono_size = size.map(px).unwrap_or(theme.mono_font_size);
        let style = GridStyle {
            font: Font {
                family,
                fallbacks: symbol_fallbacks(cx),
                ..Default::default()
            },
            font_size: window.rem_size() * (f32::from(mono_size) / 16.),
            foreground: theme.foreground,
            background,
            selection: theme.selection,
            cursor: theme.primary,
            muted: theme.muted_foreground,
            dark: theme.mode.is_dark(),
        };
        let focused = self.focus.is_focused(window);
        let marked = self.marked.clone();
        let notice = if let Some(error) = &self.error {
            Some(error.clone())
        } else if self.exited {
            Some("The shell exited. Press Enter for a new one.".into())
        } else {
            None
        };
        let term = self.shell.as_ref().map(|shell| shell.term.clone());
        let geometry = self.geometry.clone();
        let entity = cx.entity();
        let focus = self.focus.clone();

        div()
            .id("terminal")
            .key_context(TERMINAL_CONTEXT)
            .track_focus(&self.focus)
            .role(gpui_kit::Role::Terminal)
            .aria_label("Terminal")
            .size_full()
            .min_h_0()
            .bg(background)
            .px_2()
            .py_1()
            .cursor(gpui_kit::CursorStyle::IBeam)
            .on_key_down(cx.listener(Self::key_down))
            .on_action(cx.listener(Self::send_key))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::clear))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| view.mouse_up(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|view, _, _, cx| view.mouse_up(cx)),
            )
            .on_scroll(cx.listener(Self::scroll))
            .child(
                canvas(
                    {
                        let entity = entity.clone();
                        let style = style.clone();
                        move |bounds, window, cx| {
                            let cell = style.cell_size(window, cx);
                            let size = GridSize {
                                columns: (f32::from(bounds.size.width) / f32::from(cell.width))
                                    .floor()
                                    .max(1.) as usize,
                                lines: (f32::from(bounds.size.height) / f32::from(cell.height))
                                    .floor()
                                    .max(1.) as usize,
                            };
                            entity.update(cx, |view, _| view.set_size(size, cell));
                            // the lock only for reading the cells: the shell's
                            // output waits on it
                            let cells = term.as_ref().map(|term| {
                                lay_out_cells(&term.lock(), &style, bounds.origin, cell)
                            });
                            let frame = cells.map(|cells| {
                                layout_grid(cells, &style, bounds.origin, cell, focused, window)
                            });
                            (cell, size, frame)
                        }
                    },
                    move |bounds, (cell, size, frame), window, cx| {
                        let mut cursor_bounds = None;
                        if let Some(frame) = &frame {
                            for (bounds, color) in &frame.backgrounds {
                                window.paint_quad(fill(*bounds, *color));
                            }
                            for (bounds, c, color) in &frame.separators {
                                paint_separator(*bounds, *c, *color, window);
                            }
                            for line in &frame.lines {
                                let _ = line.shaped.paint(
                                    line.origin,
                                    cell.height,
                                    TextAlign::Left,
                                    None,
                                    window,
                                    cx,
                                );
                            }
                            if let Some(cursor) = frame.cursor.as_ref().filter(|_| notice.is_none())
                            {
                                cursor_bounds = Some(cursor.bounds);
                                paint_cursor(cursor, &marked, &style, focused, window, cx);
                            }
                        }
                        if let Some(notice) = &notice {
                            let run = TextRun {
                                len: notice.len(),
                                font: style.font.clone(),
                                color: style.muted,
                                background_color: None,
                                underline: None,
                                strikethrough: None,
                            };
                            let shaped = window.text_system().shape_line(
                                notice.clone(),
                                style.font_size,
                                &[run],
                                None,
                            );
                            let rows = frame.as_ref().map_or(0, |frame| frame.used_lines);
                            let y = bounds.origin.y + cell.height * (rows.min(size.lines) as f32);
                            let _ = shaped.paint(
                                point(bounds.origin.x, y),
                                cell.height,
                                TextAlign::Left,
                                None,
                                window,
                                cx,
                            );
                        }
                        geometry.set(Geometry {
                            origin: bounds.origin,
                            cell,
                            columns: size.columns,
                            lines: size.lines,
                            cursor: cursor_bounds,
                        });
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                    },
                )
                .size_full(),
            )
    }
}

// ----- drawing --------------------------------------------------------------------------------

#[derive(Clone)]
struct GridStyle {
    font: Font,
    font_size: Pixels,
    foreground: Hsla,
    background: Hsla,
    selection: Hsla,
    cursor: Hsla,
    muted: Hsla,
    dark: bool,
}

impl GridStyle {
    /// A cell: the width of the font's "m", and a line a little taller than
    /// the font, as terminals use.
    fn cell_size(&self, window: &Window, cx: &App) -> gpui_kit::Size<Pixels> {
        let _ = window;
        let text_system = cx.text_system();
        let font_id = text_system.resolve_font(&self.font);
        let width = text_system
            .advance(font_id, self.font_size, 'm')
            .map(|advance| advance.width)
            .unwrap_or(self.font_size * 0.6);
        size(width, (self.font_size * 1.3).round())
    }

    /// Whether `font` is the face `flags` ask for (bold, italic).
    fn same_face(&self, font: &Font, flags: Flags) -> bool {
        let weight = if flags.contains(Flags::BOLD) {
            FontWeight::BOLD
        } else {
            self.font.weight
        };
        let italic = flags.contains(Flags::ITALIC);
        font.weight == weight && (font.style == FontStyle::Italic) == italic
    }

    fn font(&self, flags: Flags) -> Font {
        let mut font = self.font.clone();
        if flags.contains(Flags::BOLD) {
            font.weight = FontWeight::BOLD;
        }
        if flags.contains(Flags::ITALIC) {
            font.style = FontStyle::Italic;
        }
        font
    }
}

struct ShapedRow {
    origin: Point<Pixels>,
    shaped: gpui_kit::ShapedLine,
}

struct CursorPaint {
    bounds: Bounds<Pixels>,
    shape: CursorShape,
    /// The character under a block cursor, drawn in the background colour.
    glyph: Option<(String, Font)>,
}

struct Frame {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    /// Powerline separators, drawn as shapes so no special font is needed.
    separators: Vec<(Bounds<Pixels>, char, Hsla)>,
    lines: Vec<ShapedRow>,
    cursor: Option<CursorPaint>,
    /// Rows down to the last one with text, for placing the exit notice.
    used_lines: usize,
}

/// One run of cells drawn as one shaped line.
struct Segment {
    column: usize,
    /// The column a following cell continues the segment at.
    next_column: usize,
    text: String,
    runs: Vec<TextRun>,
    /// Wide characters are shaped alone at their natural width; narrow ones
    /// are forced onto the cell grid.
    wide: bool,
}

/// The screen's cells as backgrounds, separators and segments to shape, at
/// `origin` with cells of `cell`. Needs no window, so tests can check it.
struct Cells {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    separators: Vec<(Bounds<Pixels>, char, Hsla)>,
    /// Each with its screen row.
    segments: Vec<(usize, Segment)>,
    /// The character under the cursor and its font, for a block cursor.
    cursor_glyph: Option<(String, Font)>,
    /// The cursor's cell (two for a wide character) and shape, when shown.
    cursor: Option<(Bounds<Pixels>, CursorShape)>,
    /// Rows down to the last one with text, or the cursor's.
    used_lines: usize,
}

/// The screen to paint: `cells` (laid out under the screen's lock) shaped
/// into lines, which takes the longest and so runs after the lock is let go.
fn layout_grid(
    cells: Cells,
    style: &GridStyle,
    origin: Point<Pixels>,
    cell: gpui_kit::Size<Pixels>,
    focused: bool,
    window: &mut Window,
) -> Frame {
    let Cells {
        backgrounds,
        separators,
        segments,
        cursor_glyph,
        cursor,
        used_lines,
    } = cells;
    let text_system = window.text_system().clone();
    let lines = segments
        .into_iter()
        .map(|(row, segment)| {
            let shaped = text_system.shape_line(
                segment.text.into(),
                style.font_size,
                &segment.runs,
                (!segment.wide).then_some(cell.width),
            );
            ShapedRow {
                origin: point(
                    origin.x + cell.width * segment.column as f32,
                    origin.y + cell.height * row as f32,
                ),
                shaped,
            }
        })
        .collect();
    Frame {
        backgrounds,
        separators,
        lines,
        cursor: cursor.map(|(bounds, shape)| CursorPaint {
            bounds,
            shape,
            glyph: if focused { cursor_glyph } else { None },
        }),
        used_lines,
    }
}

fn lay_out_cells<L: EventListener>(
    term: &Term<L>,
    style: &GridStyle,
    origin: Point<Pixels>,
    cell: gpui_kit::Size<Pixels>,
) -> Cells {
    let content = term.renderable_content();
    let palette = colors::Palette {
        table: content.colors,
        dark: style.dark,
        foreground: style.foreground,
        background: style.background,
    };
    let offset = content.display_offset as i32;
    let selection = content.selection;
    let cursor_point = content.cursor.point;
    let cursor_shape = content.cursor.shape;
    let show_cursor = content.mode.contains(TermMode::SHOW_CURSOR);

    let mut backgrounds: Vec<(Bounds<Pixels>, Hsla)> = Vec::new();
    let mut separators: Vec<(Bounds<Pixels>, char, Hsla)> = Vec::new();
    let mut segments: Vec<(usize, Segment)> = Vec::new();
    let mut cursor_glyph = None;
    let mut used_lines = 0;
    let push_background = |backgrounds: &mut Vec<(Bounds<Pixels>, Hsla)>,
                           row: usize,
                           column: usize,
                           width: usize,
                           color: Hsla| {
        let bounds = Bounds::new(
            point(
                origin.x + cell.width * column as f32,
                origin.y + cell.height * row as f32,
            ),
            size(cell.width * width as f32, cell.height),
        );
        // merge with the previous cell's when it continues it
        if let Some((last, last_color)) = backgrounds.last_mut()
            && *last_color == color
            && last.origin.y == bounds.origin.y
            && (last.origin.x + last.size.width - bounds.origin.x).abs() < px(0.5)
        {
            last.size.width += bounds.size.width;
            return;
        }
        backgrounds.push((bounds, color));
    };

    let mut current: Option<(usize, Segment)> = None;
    for indexed in content.display_iter {
        let row = (indexed.point.line.0 + offset) as usize;
        let column = indexed.point.column.0;
        let cell_ref = indexed.cell;
        let flags = cell_ref.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let width = if flags.contains(Flags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let (mut fg, mut bg) = (cell_ref.fg, cell_ref.bg);
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let mut fg = palette.resolve(fg);
        let bg_color = match bg {
            Color::Named(NamedColor::Background) if !flags.contains(Flags::INVERSE) => None,
            bg => Some(palette.resolve(bg)),
        };
        if flags.contains(Flags::DIM) {
            fg = fg.opacity(0.7);
        }
        if flags.contains(Flags::HIDDEN) {
            fg = bg_color.unwrap_or(style.background);
        }
        let selected = selection.is_some_and(|range| range.contains(indexed.point));
        if selected {
            push_background(&mut backgrounds, row, column, width, style.selection);
        } else if let Some(bg) = bg_color {
            push_background(&mut backgrounds, row, column, width, bg);
        }

        let is_cursor = show_cursor && indexed.point == cursor_point;
        if is_cursor && cell_ref.c != ' ' {
            cursor_glyph = Some((cell_ref.c.to_string(), style.font(flags)));
        }
        let c = cell_ref.c;
        if c == ' ' || c == '\0' {
            // spaces end a segment: nothing to shape
            if let Some(segment) = current.take() {
                segments.push(segment);
            }
            continue;
        }
        used_lines = used_lines.max(row + 1);
        if is_separator(c) {
            if let Some(segment) = current.take() {
                segments.push(segment);
            }
            let bounds = Bounds::new(
                point(
                    origin.x + cell.width * column as f32,
                    origin.y + cell.height * row as f32,
                ),
                size(cell.width, cell.height),
            );
            separators.push((bounds, c, fg));
            continue;
        }
        // the cell's text goes straight into its segment's: no string per cell
        let marks = cell_ref.zerowidth().unwrap_or_default();
        let len = c.len_utf8() + marks.iter().map(|mark| mark.len_utf8()).sum::<usize>();
        let push_text = |text: &mut String| {
            text.push(c);
            text.extend(marks);
        };
        let underline = flags
            .intersects(Flags::ALL_UNDERLINES)
            .then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(fg),
                wavy: flags.contains(Flags::UNDERCURL),
            });
        let strikethrough = flags
            .contains(Flags::STRIKEOUT)
            .then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(fg),
            });
        // the font is copied only for a run that starts here
        let run = || TextRun {
            len,
            font: style.font(flags),
            color: fg,
            background_color: None,
            underline,
            strikethrough,
        };
        let wide = width == 2;
        let continues = current.as_ref().is_some_and(|(r, segment)| {
            *r == row && !wide && !segment.wide && segment.next_column == column
        });
        if continues {
            let (_, segment) = current.as_mut().expect("checked");
            push_text(&mut segment.text);
            segment.next_column = column + 1;
            // cells styled alike share one run
            match segment.runs.last_mut() {
                Some(last)
                    if last.color == fg
                        && last.underline == underline
                        && last.strikethrough == strikethrough
                        && style.same_face(&last.font, flags) =>
                {
                    last.len += len;
                }
                _ => segment.runs.push(run()),
            }
        } else {
            if let Some(segment) = current.take() {
                segments.push(segment);
            }
            let mut text = String::new();
            push_text(&mut text);
            current = Some((
                row,
                Segment {
                    column,
                    next_column: column + width,
                    runs: vec![run()],
                    text,
                    wide,
                },
            ));
            if wide {
                segments.extend(current.take());
            }
        }
    }
    segments.extend(current.take());

    let cursor_row = cursor_point.line.0 + offset;
    let cursor = (show_cursor && cursor_row >= 0 && (cursor_row as usize) < term.screen_lines())
        .then(|| {
            let wide = term.grid()[cursor_point].flags.contains(Flags::WIDE_CHAR);
            let width = if wide { 2. } else { 1. };
            let bounds = Bounds::new(
                point(
                    origin.x + cell.width * cursor_point.column.0 as f32,
                    origin.y + cell.height * cursor_row as f32,
                ),
                size(cell.width * width, cell.height),
            );
            (bounds, cursor_shape)
        });
    Cells {
        backgrounds,
        separators,
        segments,
        cursor_glyph,
        cursor,
        used_lines: used_lines.max(cursor_row.max(0) as usize + 1),
    }
}

/// The terminal font and size of Settings (`None`: the code font's). A
/// family that is not installed is left out, since GPUI cannot lay text out
/// in it; checked here, when the setting or the fonts change, not per frame.
fn terminal_font_setting(cx: &App) -> (Option<SharedString>, Option<f32>) {
    let display = &crate::settings::get(cx).display;
    let family = display
        .terminal_font
        .clone()
        .filter(|family| crate::settings_window::font_installed(family, cx))
        .map(SharedString::from);
    (family, display.terminal_font_size)
}

/// Powerline's arrows: solid and thin, pointing right and left.
fn is_separator(c: char) -> bool {
    ('\u{e0b0}'..='\u{e0b3}').contains(&c)
}

/// Draws a Powerline arrow across its cell, as Alacritty and xterm.js do, so
/// prompts that use them look right without a patched font.
fn paint_separator(bounds: Bounds<Pixels>, c: char, color: Hsla, window: &mut Window) {
    let (left, right) = (bounds.origin.x, bounds.origin.x + bounds.size.width);
    let (top, bottom) = (bounds.origin.y, bounds.origin.y + bounds.size.height);
    let middle = top + bounds.size.height / 2.;
    // pointing right starts on the left edge; pointing left, on the right
    let (base, tip) = match c {
        '\u{e0b0}' | '\u{e0b1}' => (left, right),
        _ => (right, left),
    };
    let solid = matches!(c, '\u{e0b0}' | '\u{e0b2}');
    let mut path = if solid {
        gpui_kit::PathBuilder::fill()
    } else {
        gpui_kit::PathBuilder::stroke(px(1.))
    };
    path.move_to(point(base, top));
    path.line_to(point(tip, middle));
    path.line_to(point(base, bottom));
    if solid {
        path.close();
    }
    if let Ok(path) = path.build() {
        window.paint_path(path, color);
    }
}

/// Installed fonts with Powerline and Nerd Font symbols (prompt icons, the
/// branch mark), tried after the code font. Looked up once.
fn symbol_fallbacks(_: &App) -> Option<gpui_kit::FontFallbacks> {
    // until the font listing has come back, the code font alone
    SYMBOL_FONTS.get().cloned().flatten()
}

/// Built once, so a render copies a handle instead of a list of names.
static SYMBOL_FONTS: std::sync::OnceLock<Option<gpui_kit::FontFallbacks>> =
    std::sync::OnceLock::new();

/// Picks the symbol fonts out of the installed fonts (listed once at
/// startup, off the UI thread, by `settings_window::find_installed_fonts`).
pub fn remember_symbol_fonts(installed: &[String]) {
    let mut fonts: Vec<String> = installed
        .iter()
        .filter(|name| {
            let lower = name.to_lowercase();
            lower.contains("nerd font")
                || lower.contains("powerline")
                || lower.ends_with(" nf")
                || lower.contains(" nf ")
        })
        .cloned()
        .collect();
    fonts.sort();
    fonts.dedup();
    fonts.truncate(3);
    let _ =
        SYMBOL_FONTS.set((!fonts.is_empty()).then(|| gpui_kit::FontFallbacks::from_fonts(fonts)));
}

fn paint_cursor(
    cursor: &CursorPaint,
    marked: &Option<String>,
    style: &GridStyle,
    focused: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let bounds = cursor.bounds;
    if let Some(text) = marked {
        // the composition, underlined, over whatever is at the cursor
        let run = TextRun {
            len: text.len(),
            font: style.font.clone(),
            color: style.foreground,
            background_color: Some(style.background),
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(style.cursor),
                wavy: false,
            }),
            strikethrough: None,
        };
        let shaped =
            window
                .text_system()
                .shape_line(text.clone().into(), style.font_size, &[run], None);
        let _ = shaped.paint(
            bounds.origin,
            bounds.size.height,
            TextAlign::Left,
            None,
            window,
            cx,
        );
        return;
    }
    if !focused {
        window.paint_quad(outline(bounds, style.cursor, BorderStyle::Solid));
        return;
    }
    match cursor.shape {
        CursorShape::Beam => {
            window.paint_quad(fill(
                Bounds::new(bounds.origin, size(px(2.), bounds.size.height)),
                style.cursor,
            ));
        }
        CursorShape::Underline => {
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        bounds.origin.x,
                        bounds.origin.y + bounds.size.height - px(2.),
                    ),
                    size(bounds.size.width, px(2.)),
                ),
                style.cursor,
            ));
        }
        CursorShape::HollowBlock => {
            window.paint_quad(outline(bounds, style.cursor, BorderStyle::Solid));
        }
        CursorShape::Hidden => {}
        CursorShape::Block => {
            window.paint_quad(fill(bounds, style.cursor));
            if let Some((glyph, font)) = &cursor.glyph {
                let run = TextRun {
                    len: glyph.len(),
                    font: font.clone(),
                    color: style.background,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped = window.text_system().shape_line(
                    glyph.clone().into(),
                    style.font_size,
                    &[run],
                    None,
                );
                let _ = shaped.paint(
                    bounds.origin,
                    bounds.size.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            }
        }
    }
}

// ----- the shell ------------------------------------------------------------------------------

fn spawn_shell(cwd: &Path, size: GridSize, listener: Listener) -> anyhow::Result<Shell> {
    let term = Arc::new(FairMutex::new(Term::new(
        Config::default(),
        &size,
        listener.clone(),
    )));
    let options = tty::Options {
        working_directory: Some(cwd.to_owned()),
        env: HashMap::from([
            ("TERM".to_owned(), "xterm-256color".to_owned()),
            ("COLORTERM".to_owned(), "truecolor".to_owned()),
            ("TERM_PROGRAM".to_owned(), "Randori".to_owned()),
        ]),
        ..Default::default()
    };
    let window_size = WindowSize {
        num_lines: size.lines as u16,
        num_cols: size.columns as u16,
        cell_width: 8,
        cell_height: 16,
    };
    let pty = tty::new(&options, window_size, 0)?;
    let event_loop = EventLoop::new(term.clone(), listener, pty, false, false)?;
    let sender = event_loop.channel();
    event_loop.spawn();
    Ok(Shell { term, sender })
}

/// What the terminal reads to tell that a command changed the repository.
#[derive(Clone, Debug)]
struct RepoWatch {
    /// `op_heads/heads`: jj names its newest operations there.
    op_heads: PathBuf,
    /// The colocated Git repository: a `git` command moves its refs without
    /// a jj operation.
    git_dir: Option<PathBuf>,
}

/// The repository as [`RepoWatch`] last read it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RepoSignature {
    operations: Vec<String>,
    git: Option<GitSignature>,
}

/// `HEAD` and when the refs last moved. Git updates a ref by renaming a lock
/// file over it, which touches its directory, so directory times are enough.
#[derive(Clone, Debug, PartialEq, Eq)]
struct GitSignature {
    head: String,
    times: Vec<(PathBuf, Option<SystemTime>)>,
}

impl RepoWatch {
    /// The watch for the workspace at `root`, when it is a jj workspace.
    fn of(root: &Path) -> Option<Self> {
        let op_heads = op_heads_dir(root)?;
        let git_dir = Some(root.join(".git")).filter(|dir| dir.is_dir());
        Some(Self { op_heads, git_dir })
    }

    fn read(&self) -> RepoSignature {
        RepoSignature {
            operations: read_signature(&self.op_heads),
            git: self.git_dir.as_deref().map(read_git_signature),
        }
    }
}

/// The event for a repository read as `now` after `seen`, if it changed.
/// Nothing for a repository not read before (the first read has not come
/// back yet): `now` is only taken as seen, since the tab shows it already.
fn repository_change(seen: Option<&RepoSignature>, now: &RepoSignature) -> Option<TerminalEvent> {
    let seen = seen?;
    if seen == now {
        return None;
    }
    // jj in a colocated repository moves Git's refs too; only a change to
    // Git alone needs the snapshot that imports it
    let jj_ran = seen.operations != now.operations;
    Some(TerminalEvent::RepositoryChanged { snapshot: !jj_ran })
}

/// `op_heads/heads` of the repository the workspace at `root` uses. A
/// secondary workspace's `.jj/repo` is a file naming the repository.
fn op_heads_dir(root: &Path) -> Option<PathBuf> {
    let jj = root.join(".jj");
    let repo = jj.join("repo");
    let repo = if repo.is_file() {
        let target = std::fs::read_to_string(&repo).ok()?;
        let target = PathBuf::from(target.trim());
        if target.is_absolute() {
            target
        } else {
            jj.join(target)
        }
    } else {
        repo
    };
    let heads = repo.join("op_heads").join("heads");
    heads.is_dir().then_some(heads)
}

/// The operation heads' names: they change whenever jj records an operation.
fn read_signature(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn read_git_signature(git_dir: &Path) -> GitSignature {
    let modified = |path: &Path| std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let head = std::fs::read_to_string(git_dir.join("HEAD")).unwrap_or_default();
    let packed = git_dir.join("packed-refs");
    let mut times = vec![(packed.clone(), modified(&packed))];
    let mut dirs = vec![git_dir.join("refs")];
    while let Some(dir) = dirs.pop() {
        times.push((dir.clone(), modified(&dir)));
        if let Ok(entries) = std::fs::read_dir(&dir) {
            dirs.extend(
                entries
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                    .map(|entry| entry.path()),
            );
        }
    }
    times.sort();
    GitSignature {
        head: head.trim().to_owned(),
        times,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// gpui-kit's root moves focus on Tab (and copies on Ctrl+C off macOS);
    /// the shell must get them first, or completion and interrupts break.
    #[test]
    fn the_shell_keeps_the_keys_the_root_would_take() {
        for key in ["escape", "tab", "shift-tab"] {
            assert!(SHELL_KEYS.contains(&key), "{key} goes to the shell");
        }
        if !cfg!(target_os = "macos") {
            assert!(SHELL_KEYS.contains(&"ctrl-c"));
        }
    }

    fn test_style() -> GridStyle {
        GridStyle {
            font: gpui_kit::font("Menlo"),
            font_size: px(13.),
            foreground: gpui_kit::black(),
            background: gpui_kit::white(),
            selection: gpui_kit::blue(),
            cursor: gpui_kit::black(),
            muted: gpui_kit::black(),
            dark: false,
        }
    }

    /// The cells of a 20×3 screen after `output`, with 10×20 px cells.
    fn cells_after(output: &str) -> Cells {
        let grid = GridSize {
            columns: 20,
            lines: 3,
        };
        let mut term = Term::new(
            Config::default(),
            &grid,
            alacritty_terminal::event::VoidListener,
        );
        let mut parser: alacritty_terminal::vte::ansi::Processor =
            alacritty_terminal::vte::ansi::Processor::new();
        parser.advance(&mut term, output.as_bytes());
        lay_out_cells(
            &term,
            &test_style(),
            point(px(0.), px(0.)),
            gpui_kit::size(px(10.), px(20.)),
        )
    }

    fn texts(cells: &Cells) -> Vec<(usize, usize, &str)> {
        cells
            .segments
            .iter()
            .map(|(row, segment)| (*row, segment.column, segment.text.as_str()))
            .collect()
    }

    fn run_lengths(segment: &Segment) -> Vec<usize> {
        segment.runs.iter().map(|run| run.len).collect()
    }

    #[test]
    fn cells_styled_alike_share_one_run() {
        let cells = cells_after("abc");
        assert_eq!(texts(&cells), vec![(0, 0, "abc")]);
        assert_eq!(run_lengths(&cells.segments[0].1), vec![3]);
        assert_eq!(cells.used_lines, 1);
    }

    #[test]
    fn a_colour_or_weight_change_starts_a_run() {
        let cells = cells_after("\x1b[31mab\x1b[32mc\x1b[1md\x1b[0m");
        assert_eq!(texts(&cells), vec![(0, 0, "abcd")]);
        let runs = &cells.segments[0].1.runs;
        assert_eq!(run_lengths(&cells.segments[0].1), vec![2, 1, 1]);
        assert_ne!(runs[0].color, runs[1].color);
        assert_eq!(runs[1].color, runs[2].color);
        assert_eq!(runs[2].font.weight, FontWeight::BOLD);
    }

    #[test]
    fn an_underline_starts_and_ends_a_run() {
        let cells = cells_after("a\x1b[4mbc\x1b[24md");
        let segment = &cells.segments[0].1;
        assert_eq!(run_lengths(segment), vec![1, 2, 1]);
        assert!(segment.runs[0].underline.is_none());
        assert!(segment.runs[1].underline.is_some());
        assert!(segment.runs[2].underline.is_none());
    }

    #[test]
    fn spaces_and_wide_characters_split_segments() {
        // a wide character is shaped alone, two cells wide
        let cells = cells_after("ab cd한e");
        assert_eq!(
            texts(&cells),
            vec![(0, 0, "ab"), (0, 3, "cd"), (0, 5, "한"), (0, 7, "e")]
        );
        let wide = &cells.segments[2].1;
        assert!(wide.wide);
        assert_eq!(wide.next_column, 7);
        assert!(!cells.segments[3].1.wide);
        // multi-byte text: a run's length is in bytes
        assert_eq!(run_lengths(wide), vec!["한".len()]);
    }

    #[test]
    fn combining_marks_stay_with_their_cell() {
        // e + U+0301: one cell, three bytes, one run
        let cells = cells_after("ae\u{301}b");
        assert_eq!(texts(&cells), vec![(0, 0, "ae\u{301}b")]);
        assert_eq!(run_lengths(&cells.segments[0].1), vec![5]);
    }

    #[test]
    fn the_cursor_is_placed_with_the_cells() {
        let cells = cells_after("ab");
        let (bounds, _) = cells.cursor.expect("shown");
        assert_eq!((bounds.origin.x, bounds.origin.y), (px(20.), px(0.)));
        assert_eq!(bounds.size.width, px(10.));

        // rows down to the cursor count as used, for the exit notice
        let cells = cells_after("a\r\n\r\n");
        assert_eq!(cells.used_lines, 3);

        let cells = cells_after("\x1b[?25l");
        assert!(cells.cursor.is_none(), "a program hid it");
    }

    #[test]
    fn backgrounds_merge_along_a_row() {
        let cells = cells_after("\x1b[41mab\x1b[0m c\r\n\x1b[41md");
        let widths: Vec<_> = cells
            .backgrounds
            .iter()
            .map(|(bounds, _)| (bounds.origin.y, bounds.origin.x, bounds.size.width))
            .collect();
        assert_eq!(
            widths,
            vec![(px(0.), px(0.), px(20.)), (px(20.), px(0.), px(10.))],
            "the default background is not drawn"
        );
        assert_eq!(cells.used_lines, 2);
    }

    #[test]
    fn powerline_arrows_are_drawn_not_shaped() {
        let cells = cells_after("a\u{e0b0}b");
        assert_eq!(texts(&cells), vec![(0, 0, "a"), (0, 2, "b")]);
        assert_eq!(cells.separators.len(), 1);
        assert_eq!(cells.separators[0].1, '\u{e0b0}');
        assert_eq!(cells.separators[0].0.origin.x, px(10.));
    }

    #[test]
    fn only_git_moving_asks_for_a_snapshot() {
        let at = |operations: &[&str], head: &str| RepoSignature {
            operations: operations.iter().map(|op| (*op).to_owned()).collect(),
            git: Some(GitSignature {
                head: head.to_owned(),
                times: Vec::new(),
            }),
        };
        let seen = at(&["op1"], "ref: refs/heads/main");
        assert!(repository_change(Some(&seen), &seen).is_none());
        // before the first read: taken as seen, no reload
        assert!(repository_change(None, &seen).is_none());
        assert!(matches!(
            repository_change(Some(&seen), &at(&["op2"], "ref: refs/heads/other")),
            Some(TerminalEvent::RepositoryChanged { snapshot: false })
        ));
        assert!(matches!(
            repository_change(Some(&seen), &at(&["op1"], "ref: refs/heads/other")),
            Some(TerminalEvent::RepositoryChanged { snapshot: true })
        ));
    }

    #[test]
    fn a_git_commit_moves_the_git_signature() {
        let dir = tempfile::tempdir().unwrap();
        let git = dir.path().join(".git");
        std::fs::create_dir_all(git.join("refs/heads")).unwrap();
        std::fs::create_dir_all(dir.path().join(".jj/repo/op_heads/heads")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        let watch = RepoWatch::of(dir.path()).unwrap();
        assert_eq!(watch.git_dir.as_deref(), Some(git.as_path()));
        let before = watch.read();
        assert_eq!(before.git.as_ref().unwrap().head, "ref: refs/heads/main");

        // as git does: a new branch directory and a ref renamed into it
        std::fs::create_dir_all(git.join("refs/heads/topic")).unwrap();
        std::fs::write(git.join("refs/heads/topic/a.lock"), "0").unwrap();
        std::fs::rename(
            git.join("refs/heads/topic/a.lock"),
            git.join("refs/heads/topic/a"),
        )
        .unwrap();
        let after = watch.read();
        assert_ne!(before, after);
        assert_eq!(before.operations, after.operations);
    }

    #[test]
    fn the_operation_heads_of_a_workspace_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let heads = dir.path().join(".jj/repo/op_heads/heads");
        std::fs::create_dir_all(&heads).unwrap();
        assert_eq!(op_heads_dir(dir.path()), Some(heads.clone()));
        assert!(read_signature(&heads).is_empty());
        std::fs::write(heads.join("abc"), "").unwrap();
        assert_eq!(read_signature(&heads), vec!["abc".to_owned()]);

        // a secondary workspace names the repository in a file
        let second = dir.path().join("second");
        std::fs::create_dir_all(second.join(".jj")).unwrap();
        std::fs::write(second.join(".jj/repo"), "../../.jj/repo").unwrap();
        let found = op_heads_dir(&second).unwrap();
        assert_eq!(found.canonicalize().unwrap(), heads.canonicalize().unwrap());
    }
}
