// Randori: tauri_build only runs for the Tauri app (`gui` feature). The gpui
// frontend builds this crate without it.
#[cfg(feature = "gui")]
use tauri_build::Attributes;

// Without `gui` the docs.rs early return is the last statement.
#[cfg_attr(not(feature = "gui"), allow(clippy::needless_return))]
fn main() {
    // docs.rs mounts the source tree read-only; tauri_build writes to gen/schemas/ which would panic.
    if std::env::var_os("DOCS_RS").is_some() {
        println!("cargo:rustc-cfg=docsrs");
        return;
    }

    #[cfg(feature = "gui")]
    tauri_build::try_build(Attributes::new().capabilities_path_pattern("gen")).unwrap()
}
