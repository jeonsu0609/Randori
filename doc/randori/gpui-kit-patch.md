# gpui-kit patch

Randori builds gpui-kit 0.9.0 with a small patch to gpui-component, taken from
the fork [jeonsu0609/gpui-kit](https://github.com/jeonsu0609/gpui-kit), branch
`randori/v0.9.0`: the `v0.9.0` tag plus the three commits proposed upstream
(`diff: Add split_ratio to size the Split columns`, `diff: Keep the file
header in view when switching modes at the top` and `text_selection: Keep the
selection on a press on a control that acts on it`), each a pull request to
longbridge/gpui-kit. The workspace `Cargo.toml` points `[patch.crates-io]` at
that commit for every gpui-kit crate Randori uses (`gpui-kit`, `gpui-base`,
`gpui-component`, `gpui-component-macros`, `gpui-kit-assets`). They must come
from the same commit: gpui-component depends on its siblings by path inside
the gpui-kit repository, so patching it alone would build a second gpui-base
next to the crates.io one.

## Changes

| Commit | Change |
| --- | --- |
| `split_ratio` | `Diff::split_ratio(f32)`: the original column's share of a Split diff (default 0.5, clamped to 0.1..=0.9). Without soft wrap neither column gets narrower than its longest line; Randori wraps split diffs, so the columns share the view's width. Part of the layout key, so wrapped rows are measured again when it changes |
| header in view | `DiffState::set_mode` keeps a diff shown from its top at the top. Upstream reveals the first line's anchor, which scrolls the file header off when rows wrap. With a regression test |
| selection control | `ElementExt::text_selection_control(id)`: a press on the element neither clears the window's text selection nor starts one. gpui-kit clears it on any press before elements see it, so the line buttons over a diff's selected lines (Include lines, Split off…) found no lines when their click ran. Works in a cached view (it tests the press against a hitbox). With a test |

## Status

| Change | Pull request | State |
| --- | --- | --- |
| `split_ratio` | longbridge/gpui-kit#3883 | merged 2026-11-10 |
| header in view | longbridge/gpui-kit#3884 | merged 2026-11-10 |
| selection control | longbridge/gpui-kit#3951 | open (2026-11-12) |

The latest release, v0.9.1 (2026-11-09), has none of them. The fork is needed
until a release has all three; then step 2 below drops it. Should #3951 change
in review, the fork's commit and Randori's use of it change with it.

## Updating

1. When Randori moves to a new gpui-kit release, rebase the patch commit onto
   that release's tag in the fork (a new `randori/v<version>` branch), push it,
   and point every `rev` in `[patch.crates-io]` at the new commit.
2. Once upstream releases both changes, drop the fork: remove the
   `[patch.crates-io]` entries and use the release.
