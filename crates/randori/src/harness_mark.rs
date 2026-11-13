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
/// name; `None` when one has not been drawn.
#[cfg(feature = "screenshot")]
pub fn resolve(key: &str, cx: &gpui_kit::App) -> Option<String> {
    let (kind, points) = key.split_once(':')?;
    let marks = cx.try_global::<HarnessMarks>();
    let points = points
        .split(';')
        .map(|point| match point.strip_prefix('@') {
            Some(name) => {
                let bounds = marks?.0.get(name)?;
                let middle = bounds.center();
                Some(format!("{};{}", f32::from(middle.x), f32::from(middle.y)))
            }
            None => Some(point.to_owned()),
        })
        .collect::<Option<Vec<_>>>()?;
    Some(format!("{kind}:{}", points.join(";")))
}
