use std::path::Path;

fn main() {
    // Tauri embeds the built frontend into the binary, which makes `../dist` an
    // input to this crate — but Cargo has no way of knowing that. Without this,
    // rebuilding the interface and then running `cargo build` produces a binary
    // containing the *previous* interface, silently. That cost an afternoon
    // once and would eventually have shipped a release with a stale window in
    // it.
    watch(Path::new("../dist"));

    tauri_build::build()
}

/// Tells Cargo that every file under `path` is a build input.
///
/// A directory on its own is not enough: Cargo checks the timestamp of exactly
/// the path it is given, so editing a file inside one goes unnoticed.
fn watch(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());

    let Ok(entries) = std::fs::read_dir(path) else {
        // Not built yet. `tauri_build` will complain about that far more
        // clearly than a panic here would.
        return;
    };
    for entry in entries.flatten() {
        let child = entry.path();
        if child.is_dir() {
            watch(&child);
        } else {
            println!("cargo:rerun-if-changed={}", child.display());
        }
    }
}
