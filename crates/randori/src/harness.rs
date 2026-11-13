//! The screenshot harness (`--screenshot`, `--keys`): drives the real key
//! bindings, mouse and wheel before capturing the window, for README images,
//! visual checks and frame timings. Built with `--features screenshot` only.
//!
//! The steps of `--keys`, separated by `,` (points by `;`):
//!
//! - a keystroke, e.g. `down`, `cmd-enter`, `x`
//! - `action:<namespace::Name>`: an action run as a menu item runs it,
//!   through the focused element
//! - `click:X;Y`, `cmdclick:X;Y` (with ⌘), `rclick:X;Y`, `dblclick:X;Y` and
//!   `drag:X1;Y1;X2;Y2`: the mouse pressed at window points
//! - `move:X;Y`: the pointer moved there, no button, to hover
//! - `scroll:X;Y;DY`: the wheel turned DY points there
//! - in any of these pointer steps, a point can be `@name` instead: the
//!   middle of the element marked `name` (see `harness_mark`), e.g.
//!   `click:@summary`, `scroll:@name;-200`
//! - `bench:X;Y;DY;N`: the wheel turned N times, each frame drawn after it
//!   (render, layout and paint) timed; prints the median and the slowest

use std::path::PathBuf;

use gpui_kit::{AnyWindowHandle, AsyncApp, Entity};

use crate::app_view::RandoriApp;
use crate::settings_window;
use crate::wait_until_settled;

/// The steps whose points `pointer_events` reads, so whose points can be
/// `@name`.
const POINTER_STEPS: [&str; 7] = [
    "click", "cmdclick", "rclick", "dblclick", "drag", "move", "scroll",
];

/// Runs `keys` on `window`, then saves the window (or Settings', with
/// `RANDORI_SETTINGS_PAGE`) to `path` and quits.
pub async fn run(
    cx: &mut AsyncApp,
    app: &Entity<RandoriApp>,
    window: AnyWindowHandle,
    keys: Vec<String>,
    path: PathBuf,
) {
    // for a run that went wrong elsewhere (CI): the window it had
    let _ = window.update(cx, |_, window, _| {
        eprintln!(
            "keys: window {:?}, scale {}",
            window.viewport_size(),
            window.scale_factor()
        );
    });
    // Drive the real key bindings, as a user would, before capturing.
    for key in keys {
        if let Some(spec) = key.strip_prefix("bench:") {
            let _ = window.update(cx, |_, window, cx| bench_scroll(spec, window, cx));
            wait_until_settled(cx, app).await;
            continue;
        }
        // points named by an element (`click:@summary`), where it is drawn
        // now: the marks are made again by a frame drawn whole (`refresh`
        // passes the views' caches), so one no longer drawn has none
        // (only a pointer step's: a keystroke can be `@` itself)
        let named = key
            .split_once(':')
            .is_some_and(|(kind, points)| POINTER_STEPS.contains(&kind) && points.contains('@'));
        let key = if named {
            // Drawn until two frames agree: the first frame of a window can
            // lay its panels out before their sizes settle (a window the
            // screen shrank), and an element would be pressed where it was.
            let resolved = window.update(cx, |_, window, cx| {
                let mut draw = || {
                    crate::harness_mark::forget(cx);
                    window.refresh();
                    window.draw(cx).clear(cx);
                    crate::harness_mark::resolve(&key, cx)
                };
                let mut last = draw();
                for _ in 0..4 {
                    let next = draw();
                    if next == last {
                        break;
                    }
                    last = next;
                }
                last
            });
            // a step that cannot be taken fails the run, rather than going
            // on to fail somewhere it does not explain
            match resolved {
                Ok(Some(resolved)) => {
                    eprintln!("keys: {key} is {resolved}");
                    resolved
                }
                Ok(None) => {
                    eprintln!("keys: nothing drawn is named in {key}");
                    std::process::exit(2);
                }
                Err(err) => {
                    eprintln!("keys: the window could not be drawn for {key}: {err:#}");
                    std::process::exit(2);
                }
            }
        } else {
            key
        };
        if let Some(events) = pointer_events(&key) {
            for event in events {
                let _ = window.update(cx, |_, window, cx| {
                    // hit testing uses the last frame drawn,
                    // which a locked screen does not update
                    window.refresh();
                    window.draw(cx).clear(cx);
                    window.dispatch_event(event, cx);
                });
                wait_until_settled(cx, app).await;
            }
            continue;
        }
        if let Some(name) = key.strip_prefix("action:") {
            let _ = window.update(cx, |_, window, cx| match cx.build_action(name, None) {
                Ok(action) => window.dispatch_action(action, cx),
                Err(err) => eprintln!("keys: {err}"),
            });
            wait_until_settled(cx, app).await;
            continue;
        }
        let Ok(keystroke) = gpui_kit::Keystroke::parse(Box::leak(key.into_boxed_str())) else {
            eprintln!("keys: cannot parse a key");
            continue;
        };
        let _ = window.update(cx, |_, window, cx| {
            // focus and key handlers come from the last frame
            // drawn, which a locked screen does not update
            window.refresh();
            window.draw(cx).clear(cx);
            window.dispatch_keystroke(keystroke, cx);
        });
        wait_until_settled(cx, app).await;
    }
    let selected = app
        .read_with(cx, |app, _| app.active_view())
        .and_then(|view| view.read_with(cx, |v, _| v.selected_row()));
    println!("selected row: {selected:?}");
    // Let layout and the diff settle for a few frames before capturing.
    cx.background_executor()
        .timer(std::time::Duration::from_millis(800))
        .await;
    // `RANDORI_SETTINGS_PAGE` captures the Settings window instead
    let shot = std::env::var_os("RANDORI_SETTINGS_PAGE")
        .and_then(|_| cx.update(|cx| settings_window::window_handle(cx)))
        .unwrap_or(window);
    let _ = shot.update(cx, |_, window, cx| {
        // a frame drawn now: with the screen locked or the
        // display asleep macOS asks for none, and the last one
        // drawn would be the first
        window.refresh();
        window.draw(cx).clear(cx);
        match window.render_to_image() {
            Ok(image) => match image.save(&path) {
                Ok(()) => println!("screenshot: {}", path.display()),
                Err(err) => eprintln!("screenshot: {err}"),
            },
            Err(err) => eprintln!("screenshot: {err:#}"),
        }
        cx.quit();
    });
}

/// The `bench:X;Y;DY;N` step: N wheel turns of DY points at X;Y, each
/// followed by a frame drawn and timed. Prints the median and the slowest.
fn bench_scroll(spec: &str, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App) {
    let numbers: Result<Vec<f32>, _> = spec.split(';').map(|n| n.trim().parse()).collect();
    let Ok(&[x, y, dy, n]) = numbers.as_deref() else {
        eprintln!("bench: expected X;Y;DY;N");
        return;
    };
    let position = gpui_kit::point(gpui_kit::px(x), gpui_kit::px(y));
    window.refresh();
    window.draw(cx).clear(cx);
    let mut times = Vec::new();
    for _ in 0..n as usize {
        window.dispatch_event(
            gpui_kit::PlatformInput::Scroll(gpui_kit::ScrollEvent {
                position,
                delta: gpui_kit::ScrollDelta::Pixels(gpui_kit::point(
                    gpui_kit::px(0.),
                    gpui_kit::px(dy),
                )),
                ..Default::default()
            }),
            cx,
        );
        let start = std::time::Instant::now();
        window.draw(cx).clear(cx);
        times.push(start.elapsed().as_secs_f64() * 1000.);
    }
    times.sort_by(f64::total_cmp);
    if let (Some(median), Some(max)) = (times.get(times.len() / 2), times.last()) {
        println!(
            "bench: {} frames, median {median:.2} ms, max {max:.2} ms",
            times.len()
        );
    }
}

/// Mouse events for a `--keys` pointer step, in window points.
fn pointer_events(key: &str) -> Option<Vec<gpui_kit::PlatformInput>> {
    use gpui_kit::{
        Modifiers, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PlatformInput, point,
        px,
    };
    let (kind, coords) = key.split_once(':')?;
    let numbers: Vec<f32> = coords
        .split(';')
        .map(|n| n.trim().parse().ok())
        .collect::<Option<_>>()?;
    let at = |x: f32, y: f32| point(px(x), px(y));
    // `cmdclick` holds ⌘ (the screenshot build is macOS only)
    let modifiers = if kind == "cmdclick" {
        Modifiers {
            platform: true,
            ..Modifiers::default()
        }
    } else {
        Modifiers::default()
    };
    let down_with = |button, position, click_count| {
        PlatformInput::MouseDown(MouseDownEvent {
            button,
            position,
            modifiers,
            click_count,
            first_mouse: false,
        })
    };
    let down = |position, click_count| down_with(MouseButton::Left, position, click_count);
    let up_with = |button, position, click_count| {
        PlatformInput::MouseUp(MouseUpEvent {
            button,
            position,
            modifiers,
            click_count,
        })
    };
    let up = |position, click_count| up_with(MouseButton::Left, position, click_count);
    let moved = |position, pressed| {
        PlatformInput::MouseMove(MouseMoveEvent {
            position,
            pressed_button: pressed,
            modifiers: Modifiers::default(),
        })
    };
    match (kind, numbers.as_slice()) {
        ("click" | "cmdclick", [x, y]) => {
            let p = at(*x, *y);
            Some(vec![moved(p, None), down(p, 1), up(p, 1)])
        }
        // the pointer moves there, no button: hovers
        ("move", [x, y]) => Some(vec![moved(at(*x, *y), None)]),
        // the wheel turns at X;Y by DY points (negative scrolls down)
        ("scroll", [x, y, dy]) => Some(vec![
            moved(at(*x, *y), None),
            PlatformInput::Scroll(gpui_kit::ScrollEvent {
                position: at(*x, *y),
                delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(*dy))),
                ..Default::default()
            }),
        ]),
        ("rclick", [x, y]) => {
            let p = at(*x, *y);
            Some(vec![
                moved(p, None),
                down_with(MouseButton::Right, p, 1),
                up_with(MouseButton::Right, p, 1),
            ])
        }
        ("dblclick", [x, y]) => {
            let p = at(*x, *y);
            Some(vec![
                moved(p, None),
                down(p, 1),
                up(p, 1),
                down(p, 2),
                up(p, 2),
            ])
        }
        ("drag", [x1, y1, x2, y2]) => {
            let (from, to) = (at(*x1, *y1), at(*x2, *y2));
            let middle = at((x1 + x2) / 2., (y1 + y2) / 2.);
            let pressed = Some(MouseButton::Left);
            Some(vec![
                moved(from, None),
                down(from, 1),
                moved(middle, pressed),
                moved(to, pressed),
                up(to, 1),
            ])
        }
        _ => None,
    }
}
