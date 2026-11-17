//! Applications that show a file's change side by side, for "Open in diff
//! tool": Settings offers the ones installed, and the chosen one is opened
//! with the file's two versions (`ExternalDiff::tool`). None chosen, the
//! diff is Randori's own (the full view). jj's own
//! `ui.diff-formatter` is not used for this: it is how `jj diff` prints a
//! change in a terminal (difftastic, `:color-words`), and setting a window
//! there would open one for every `jj diff`.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// One application: its id (kept in the settings), its name, the programs
/// that open it, and their arguments, `$left` and `$right` the two files.
struct DiffApp {
    id: &'static str,
    name: &'static str,
    /// In order: whole paths (`{VAR}` an environment variable), or names
    /// looked for on `PATH`.
    programs: &'static [&'static str],
    args: &'static [&'static str],
}

const TWO_FILES: &[&str] = &["$left", "$right"];

#[cfg(target_os = "macos")]
const APPS: &[DiffApp] = &[
    DiffApp {
        id: "vscode",
        name: "VS Code",
        programs: &[
            "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
            "{HOME}/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
            "code",
        ],
        args: &["--diff", "$left", "$right"],
    },
    DiffApp {
        id: "vscodium",
        name: "VSCodium",
        programs: &[
            "/Applications/VSCodium.app/Contents/Resources/app/bin/codium",
            "codium",
        ],
        args: &["--diff", "$left", "$right"],
    },
    DiffApp {
        id: "filemerge",
        name: "FileMerge",
        // Xcode's: the /usr/bin one asks to install the developer tools
        programs: &["/Applications/Xcode.app/Contents/Developer/usr/bin/opendiff"],
        args: TWO_FILES,
    },
    DiffApp {
        id: "kaleidoscope",
        name: "Kaleidoscope",
        programs: &[
            "/Applications/Kaleidoscope.app/Contents/MacOS/ksdiff",
            "ksdiff",
        ],
        args: TWO_FILES,
    },
    DiffApp {
        id: "bcompare",
        name: "Beyond Compare",
        programs: &["/Applications/Beyond Compare.app/Contents/MacOS/bcomp"],
        args: TWO_FILES,
    },
    DiffApp {
        id: "meld",
        name: "Meld",
        programs: &["/Applications/Meld.app/Contents/MacOS/Meld", "meld"],
        args: TWO_FILES,
    },
    DiffApp {
        id: "kdiff3",
        name: "KDiff3",
        programs: &["/Applications/kdiff3.app/Contents/MacOS/kdiff3", "kdiff3"],
        args: TWO_FILES,
    },
];

#[cfg(target_os = "windows")]
const APPS: &[DiffApp] = &[
    DiffApp {
        id: "vscode",
        name: "VS Code",
        programs: &[
            "{LOCALAPPDATA}\\Programs\\Microsoft VS Code\\bin\\code.cmd",
            "{ProgramFiles}\\Microsoft VS Code\\bin\\code.cmd",
            "code.cmd",
        ],
        args: &["--diff", "$left", "$right"],
    },
    DiffApp {
        id: "vscodium",
        name: "VSCodium",
        programs: &[
            "{LOCALAPPDATA}\\Programs\\VSCodium\\bin\\codium.cmd",
            "{ProgramFiles}\\VSCodium\\bin\\codium.cmd",
            "codium.cmd",
        ],
        args: &["--diff", "$left", "$right"],
    },
    DiffApp {
        id: "winmerge",
        name: "WinMerge",
        programs: &[
            "{ProgramFiles}\\WinMerge\\WinMergeU.exe",
            "{LOCALAPPDATA}\\Programs\\WinMerge\\WinMergeU.exe",
        ],
        // not added to its recently used files
        args: &["/u", "$left", "$right"],
    },
    DiffApp {
        id: "bcompare",
        name: "Beyond Compare",
        programs: &[
            "{ProgramFiles}\\Beyond Compare 5\\BComp.exe",
            "{ProgramFiles}\\Beyond Compare 4\\BComp.exe",
        ],
        args: TWO_FILES,
    },
    DiffApp {
        id: "meld",
        name: "Meld",
        programs: &[
            "{ProgramFiles}\\Meld\\Meld.exe",
            "{ProgramFiles(x86)}\\Meld\\Meld.exe",
        ],
        args: TWO_FILES,
    },
    DiffApp {
        id: "kdiff3",
        name: "KDiff3",
        programs: &["{ProgramFiles}\\KDiff3\\kdiff3.exe"],
        args: TWO_FILES,
    },
];

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const APPS: &[DiffApp] = &[
    DiffApp {
        id: "vscode",
        name: "VS Code",
        programs: &["code"],
        args: &["--diff", "$left", "$right"],
    },
    DiffApp {
        id: "vscodium",
        name: "VSCodium",
        programs: &["codium"],
        args: &["--diff", "$left", "$right"],
    },
    DiffApp {
        id: "meld",
        name: "Meld",
        programs: &["meld"],
        args: TWO_FILES,
    },
    DiffApp {
        id: "kdiff3",
        name: "KDiff3",
        programs: &["kdiff3"],
        args: TWO_FILES,
    },
    DiffApp {
        id: "bcompare",
        name: "Beyond Compare",
        programs: &["bcompare"],
        args: TWO_FILES,
    },
];

/// The applications installed, as `(id, name)`: looked for once, as
/// Settings' pages are built again whenever a setting changes.
pub fn installed() -> &'static [(&'static str, &'static str)] {
    static INSTALLED: OnceLock<Vec<(&'static str, &'static str)>> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        APPS.iter()
            .filter(|app| program(app).is_some())
            .map(|app| (app.id, app.name))
            .collect()
    })
}

/// The name of application `id`, if Randori knows it.
pub fn name(id: &str) -> Option<&'static str> {
    APPS.iter().find(|app| app.id == id).map(|app| app.name)
}

/// The program and arguments that open application `id` on two files, if
/// it is installed now.
pub fn command(id: &str) -> Option<Vec<String>> {
    let app = APPS.iter().find(|app| app.id == id)?;
    let program = program(app)?;
    Some(
        std::iter::once(program.to_string_lossy().into_owned())
            .chain(app.args.iter().map(|arg| (*arg).to_owned()))
            .collect(),
    )
}

/// The first of `app`'s programs that is there.
fn program(app: &DiffApp) -> Option<PathBuf> {
    app.programs.iter().find_map(|program| {
        if program.contains(['/', '\\']) {
            Some(expand(program)?).filter(|path| path.is_file())
        } else {
            on_path(program)
        }
    })
}

/// `{VAR}` in `path` replaced by the environment variable; `None` when one
/// is not set.
fn expand(path: &str) -> Option<PathBuf> {
    let mut out = String::new();
    let mut rest = path;
    while let Some(start) = rest.find('{') {
        let end = start + rest[start..].find('}')?;
        out.push_str(&rest[..start]);
        out.push_str(&std::env::var(&rest[start + 1..end]).ok()?);
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    Some(PathBuf::from(out))
}

/// `name` in a folder of `PATH`. An application opened from the Finder has
/// the system's `PATH` only, so Homebrew's folders are looked in too.
fn on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let homebrew = ["/opt/homebrew/bin", "/usr/local/bin"].map(Path::new);
    std::env::split_paths(&path)
        .chain(
            homebrew
                .into_iter()
                .filter(|_| cfg!(unix))
                .map(Path::to_path_buf),
        )
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_has_its_two_files() {
        for app in APPS {
            assert!(app.args.contains(&"$left"), "{}", app.id);
            assert!(app.args.contains(&"$right"), "{}", app.id);
            assert_eq!(name(app.id), Some(app.name));
        }
    }

    #[test]
    fn variables_are_expanded() {
        // SAFETY: a variable of this test's own, read back at once
        unsafe { std::env::set_var("RANDORI_TEST_DIR", "/x") };
        assert_eq!(
            expand("{RANDORI_TEST_DIR}/a/b"),
            Some(PathBuf::from("/x/a/b"))
        );
        assert_eq!(expand("{RANDORI_NOT_SET_ANYWHERE}/a"), None);
        assert_eq!(expand("/plain"), Some(PathBuf::from("/plain")));
    }

    #[test]
    fn an_unknown_app_has_no_command() {
        assert_eq!(command("no-such-app"), None);
    }
}
