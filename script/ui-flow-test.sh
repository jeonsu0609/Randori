#!/usr/bin/env bash
# Runs the commit flow through the app, as a user would with keys and mouse,
# and checks what jj recorded. It drives the real key bindings with the
# screenshot harness (`--features screenshot`, macOS only), on a throwaway
# repository; your settings and saved tabs are not touched.
#
#   script/ui-flow-test.sh
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build -p randori --features screenshot
bin="$PWD/target/debug/randori"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
export JJ_USER="Randori Test" JJ_EMAIL="test@example.com" XDG_CONFIG_HOME="$work/config"

# On a failure, the app's last screen and what it printed (the harness says
# where each `@name` was) go to $UI_FLOW_ARTIFACTS when it is set (CI keeps
# them), and the printed lines are shown.
fail() {
    echo "ui-flow-test: $*" >&2
    if [ -f "$work/app.err" ]; then
        grep '^keys:' "$work/app.err" >&2 || true
    fi
    if [ -n "${UI_FLOW_ARTIFACTS:-}" ]; then
        mkdir -p "$UI_FLOW_ARTIFACTS"
        cp "$work/shot.png" "$work/app.err" "$UI_FLOW_ARTIFACTS/" 2>/dev/null || true
    fi
    exit 1
}

# A described base with a.txt, and a working copy that changes it and adds c.txt.
fresh_repo() {
    rm -rf "$work/repo"
    mkdir -p "$work/repo"
    (
        cd "$work/repo"
        jj git init >/dev/null 2>&1
        printf 'a\n' >a.txt
        jj describe -m base >/dev/null 2>&1
        jj new >/dev/null 2>&1
        printf 'a\nb\n' >a.txt
        printf 'c\n' >c.txt
    )
}

# Runs the app on the repository with row $2 selected (the working copy by
# default), then the keys $1.
# The window has the size the points below are for; a screen too small for it
# would shrink it and move them.
# A run that never settles (a window that is not drawn, as on a locked
# screen) is stopped after two minutes instead of holding the test, or CI.
run_app() {
    "$bin" "$work/repo" --size 1440x900 --select-row "${2:-0}" --keys "$1" \
        --screenshot "$work/shot.png" >/dev/null 2>"$work/app.err" &
    local app=$!
    # (its own notice that the sleep was stopped goes nowhere)
    (sleep 120 && kill "$app") 2>/dev/null &
    local watchdog=$!
    # not a job of this shell: stopping it prints nothing
    disown "$watchdog"
    wait "$app" || fail "the app did not finish '$1' (a step it could not take, or a locked screen)"
    # the watchdog and the sleep it waits on
    pkill -P "$watchdog" 2>/dev/null || true
    kill "$watchdog" 2>/dev/null || true
}

# jj in the repository, which names files relative to it
jj_in() {
    (cd "$work/repo" && jj --ignore-working-copy "$@" 2>/dev/null)
}

changed() {
    jj_in diff -r "$1" --name-only | sort | tr '\n' ' '
}

ops() {
    jj_in op log --no-graph -T 'id ++ "\n"' | wc -l
}

# Nothing included: a summary typed (D focuses it) commits every change, as
# `jj commit` does, and a new empty working copy starts on it.
fresh_repo
run_app 'd,m,s,g,cmd-enter'
[ "$(jj_in log -r @- --no-graph -T 'description.first_line()')" = msg ] ||
    fail "the commit is not described 'msg'"
[ "$(changed @-)" = "a.txt c.txt " ] || fail "the commit has '$(changed @-)'"
[ -z "$(changed @)" ] || fail "the working copy still has '$(changed @)'"
[ "$(jj_in log -r @-- --no-graph -T 'description.first_line()')" = base ] ||
    fail "the commit is not on base"
echo "ui-flow-test: commit every change: ok"

# A summary, Enter to the description, a description: one message with a
# blank line between them.
fresh_repo
run_app 'd,f,i,x,enter,w,h,y,cmd-enter'
# as `jj commit -m fix -m why` writes it, ending with a newline
[ "$(jj_in log -r @- --no-graph -T 'description.escape_json()')" = '"fix\n\nwhy\n"' ] ||
    fail "the commit is described $(jj_in log -r @- --no-graph -T 'description.escape_json()')"
echo "ui-flow-test: summary and description: ok"

# Including is the tab's own: jj records nothing until the commit. (The
# working copy is snapshotted first, as the app would on opening it.)
fresh_repo
(cd "$work/repo" && jj status >/dev/null 2>&1)
before="$(ops)"
run_app 'action:randori::IncludeAllChanges,action:randori::ExcludeAllChanges'
[ "$(ops)" = "$before" ] || fail "including changes wrote to the repository"
echo "ui-flow-test: including writes nothing: ok"

# c.txt's box ticked: the commit has c.txt, the new working copy a.txt.
# `@name` is an element the app marks (see harness_mark.rs).
fresh_repo
run_app 'click:@include:c.txt,d,c,cmd-enter'
[ "$(changed @-)" = "c.txt " ] || fail "the commit has '$(changed @-)'"
[ "$(changed @)" = "a.txt " ] || fail "the working copy has '$(changed @)'"
echo "ui-flow-test: commit an included file: ok"

# ⌘-click on c.txt's name ticks its box, as Split's pick does elsewhere.
fresh_repo
run_app 'cmdclick:@file:c.txt,click:@summary,c,cmd-enter'
[ "$(changed @-)" = "c.txt " ] || fail "the commit has '$(changed @-)'"
[ "$(changed @)" = "a.txt " ] || fail "the working copy has '$(changed @)'"
echo "ui-flow-test: ⌘-click includes a file: ok"

# Two of four files ticked (c.txt and e.txt, with d.txt between them left
# out): the commit has those two, the new working copy the other two.
fresh_repo
(cd "$work/repo" && printf 'd\n' >d.txt && printf 'e\n' >e.txt)
run_app 'click:@include:c.txt,click:@include:e.txt,click:@summary,t,w,o,cmd-enter'
[ "$(changed @-)" = "c.txt e.txt " ] || fail "the commit has '$(changed @-)'"
[ "$(jj_in log -r @- --no-graph -T 'description.first_line()')" = two ] ||
    fail "the commit is not described 'two'"
[ "$(changed @)" = "a.txt d.txt " ] || fail "the working copy has '$(changed @)'"
echo "ui-flow-test: commit two of four files: ok"

# Split off, as `jj split c.txt`: c.txt goes with the message typed and the
# change id; the working copy keeps a.txt, its description and its bookmark.
fresh_repo
(cd "$work/repo" && jj describe -m wip >/dev/null 2>&1 && jj bookmark create -r @ topic >/dev/null 2>&1)
change="$(jj_in log -r @ --no-graph -T 'change_id')"
run_app 'click:@include:c.txt,click:@summary,cmd-a,f,i,x,click:@split-off'
[ "$(changed @-)" = "c.txt " ] || fail "the split-off revision has '$(changed @-)'"
[ "$(jj_in log -r @- --no-graph -T 'description.first_line()')" = fix ] ||
    fail "the split-off revision is not described 'fix'"
[ "$(jj_in log -r @- --no-graph -T 'change_id')" = "$change" ] ||
    fail "the split-off revision has not kept the change id"
[ "$(changed @)" = "a.txt " ] || fail "the working copy has '$(changed @)'"
[ "$(jj_in log -r @ --no-graph -T 'description.first_line()')" = wip ] ||
    fail "the working copy lost its description"
[ "$(jj_in log -r topic --no-graph -T 'change_id')" = "$(jj_in log -r @ --no-graph -T 'change_id')" ] ||
    fail "the bookmark is not on the working copy"
echo "ui-flow-test: split off what is included: ok"

# Drag over two line numbers, Include lines, commit: the commit has those
# lines, the new working copy the rest of the file. The drag is over the
# new line numbers of lines 2 and 3, 25 points left of their boxes
# (`@line:new:2-25+0`), so it follows the diff wherever it is drawn; the
# click on Include lines is the one gpui-kit used to clear the selection for
# (see doc/randori/gpui-kit-patch.md).
rm -rf "$work/repo"
mkdir -p "$work/repo"
(
    cd "$work/repo"
    jj git init >/dev/null 2>&1
    printf 'one\ntwo\nthree\n' >a.txt
    jj describe -m base >/dev/null 2>&1
    jj new >/dev/null 2>&1
    printf 'one\nONE-A\nONE-B\ntwo\nthree\nTHREE-A\n' >a.txt
)
# (D is the log's key: after the drag the summary is clicked instead.)
run_app 'drag:@line:new:2-25+0;@line:new:3-25+0,click:@include-lines,click:@summary,l,cmd-enter'
added() {
    jj_in diff -r "$1" --git | grep '^+[^+]' | tr '\n' ' '
}
[ "$(added @-)" = "+ONE-A +ONE-B " ] || fail "the commit has '$(added @-)'"
[ "$(added @)" = "+THREE-A " ] || fail "the working copy has '$(added @)'"
echo "ui-flow-test: commit included lines: ok"

# A description with no blank line under its summary reads back with one;
# ⌘↩ with nothing typed saves nothing.
fresh_repo
(cd "$work/repo" && jj describe -r @- -m "$(printf 'a\nb')" >/dev/null 2>&1 && jj status >/dev/null 2>&1)
before="$(ops)"
run_app 'd,cmd-enter' 1
[ "$(ops)" = "$before" ] ||
    fail "the description was saved again as '$(jj_in log -r @- --no-graph -T description)'"
echo "ui-flow-test: an unchanged description is not saved: ok"
