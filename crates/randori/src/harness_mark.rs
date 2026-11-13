//! Names elements for the screenshot harness, so that a step says what it
//! presses (`click:@summary`) instead of where: a layout change then moves
//! the point with the element. Only the screenshot build records anything;
//! elsewhere a mark is the element as it was.

use gpui_kit::{ParentElement, SharedString};

/// Where each named element was last drawn.
#[cfg(feature = "screenshot")]
#[derive(Default)]
pub struct HarnessMarks(
    pub std::collections::HashMap<SharedString, gpui_kit::Bounds<gpui_kit::Pixels>>,
);

#[cfg(feature = "screenshot")]
impl gpui_kit::Global for HarnessMarks {}

pub trait HarnessMark: ParentElement + Sized {
    /// Records where this element is drawn under the name `name` makes, for
    /// `@name` in a harness step. The name is made only in the screenshot
    /// build, so another pays nothing for a `format!`.
    fn harness_mark<N: Into<SharedString>>(self, name: impl FnOnce() -> N) -> Self {
        #[cfg(feature = "screenshot")]
        {
            use gpui_kit::base::ElementExt as _;
            let name = name().into();
            self.on_prepaint(move |bounds, _, cx| {
                cx.default_global::<HarnessMarks>().0.insert(name, bounds);
            })
        }
        #[cfg(not(feature = "screenshot"))]
        {
            let _ = name;
            self
        }
    }
}

impl<T: ParentElement> HarnessMark for T {}

/// Forgets every mark, before a frame drawn whole makes them again.
#[cfg(feature = "screenshot")]
pub fn forget(cx: &mut gpui_kit::App) {
    if let Some(marks) = cx.try_global::<HarnessMarks>()
        && !marks.0.is_empty()
    {
        cx.set_global(HarnessMarks::default());
    }
}

/// `key` with each `@name` among its points (`click:@summary`,
/// `drag:@a;@b`) turned into the middle of the element drawn under that
/// name, moved by an offset when the name ends in one (`@line:new:2-40+0`,
/// 40 points left of it); `None` when one has not been drawn.
#[cfg(feature = "screenshot")]
pub fn resolve(key: &str, cx: &gpui_kit::App) -> Option<String> {
    let (kind, points) = key.split_once(':')?;
    let marks = cx.try_global::<HarnessMarks>();
    let points = points
        .split(';')
        .map(|point| match point.strip_prefix('@') {
            Some(named) => {
                let (name, (dx, dy)) = split_offset(named);
                let bounds = marks?.0.get(name)?;
                let middle = bounds.center();
                Some(format!(
                    "{};{}",
                    f32::from(middle.x) + dx,
                    f32::from(middle.y) + dy
                ))
            }
            None => Some(point.to_owned()),
        })
        .collect::<Option<Vec<_>>>()?;
    Some(format!("{kind}:{}", points.join(";")))
}

/// A mark's name and the offset after it: `name+DX+DY` (either sign), or
/// the name alone and no offset.
#[cfg(any(feature = "screenshot", test))]
fn split_offset(named: &str) -> (&str, (f32, f32)) {
    // the two signed numbers at the end, the last first
    fn signed_tail(s: &str) -> Option<(&str, f32)> {
        let at = s.rfind(['+', '-'])?;
        let number = s[at + 1..].parse::<f32>().ok()?;
        let number = if s[at..].starts_with('-') {
            -number
        } else {
            number
        };
        Some((&s[..at], number))
    }
    signed_tail(named)
        .and_then(|(rest, dy)| {
            let (name, dx) = signed_tail(rest)?;
            (!name.is_empty()).then_some((name, (dx, dy)))
        })
        .unwrap_or((named, (0., 0.)))
}

#[cfg(test)]
mod tests {
    use super::split_offset;

    #[test]
    fn a_name_may_end_in_an_offset() {
        assert_eq!(split_offset("summary"), ("summary", (0., 0.)));
        assert_eq!(split_offset("line:new:2-40+0"), ("line:new:2", (-40., 0.)));
        assert_eq!(
            split_offset("include:a-b.txt"),
            ("include:a-b.txt", (0., 0.))
        );
        assert_eq!(split_offset("x+1.5-2"), ("x", (1.5, -2.)));
    }
}
