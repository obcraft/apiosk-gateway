//! Work spawned while serving a request must keep the request's trace, so
//! request-path code spawns through `telemetry::spawn`, never `tokio::spawn`.

use std::path::{Path, PathBuf};

const BARE_SPAWNS: [&str; 4] = ["tokio::spawn(", "tokio::task::spawn(", "use tokio::spawn;", "use tokio::task::spawn;"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn request_path_code_never_spawns_outside_the_current_trace() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src.join("engine"), &mut files);
    for name in ["engine.rs", "execution.rs", "public_api.rs"] {
        files.push(src.join(name));
    }
    for entry in std::fs::read_dir(&src).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("server") && name.ends_with(".rs")) {
            files.push(path);
        }
    }
    assert!(files.len() > 10, "the scan must cover the engine and server modules");
    let offenders: Vec<String> = files
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).unwrap();
            let name = path.strip_prefix(&src).unwrap().display().to_string();
            text.lines()
                .enumerate()
                .filter(|(_, line)| BARE_SPAWNS.iter().any(|spawn| line.contains(spawn)))
                .map(|(number, _)| format!("src/{name}:{}", number + 1))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(offenders.is_empty(), "use crate::telemetry::spawn so the trace follows the work: {offenders:?}");
}
