//! Source hygiene: no invisible control bytes in text sources.
//!
//! Tool-generated files have accidentally embedded literal NUL/SOH bytes
//! more than once; they render as invisible glyphs, make Git classify the
//! file as binary (NUL), and derail review. Everything except TAB/LF/CR is
//! banned from the trees that hold hand-maintained or emitted-as-text code.

use std::path::{Path, PathBuf};

const SCAN_ROOTS: &[&str] = &["src", "runtime", "e2e", "tests"];
const TEXT_EXTS: &[&str] = &[
    "rs", "go", "ts", "py", "rb", "mjs", "js", "toml", "md", "yml", "yaml", "json", "sh", "mod",
];

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable dir") {
        let path = entry.expect("dir entry").path();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.starts_with('.') || name == "node_modules" || name == "target" {
            continue;
        }
        if path.is_dir() {
            collect(&path, out);
        } else if path
            .extension()
            .is_some_and(|e| TEXT_EXTS.contains(&e.to_string_lossy().as_ref()))
        {
            out.push(path);
        }
    }
}

#[test]
fn text_sources_contain_no_control_bytes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    for scan_root in SCAN_ROOTS {
        collect(&root.join(scan_root), &mut files);
    }
    // Root-level text files too (redwood.toml, *.md, api-spec.yml, ...).
    for entry in std::fs::read_dir(root).expect("readable root") {
        let path = entry.expect("dir entry").path();
        if path.is_file()
            && path
                .extension()
                .is_some_and(|e| TEXT_EXTS.contains(&e.to_string_lossy().as_ref()))
        {
            files.push(path);
        }
    }
    assert!(
        files.len() > 50,
        "scan looks broken: only {} files",
        files.len()
    );

    let mut offenders = Vec::new();
    for file in &files {
        let bytes = std::fs::read(file).expect("readable file");
        for (i, &b) in bytes.iter().enumerate() {
            if b < 0x20 && b != b'\t' && b != b'\n' && b != b'\r' {
                let line = bytes[..i].iter().filter(|&&c| c == b'\n').count() + 1;
                offenders.push(format!("{}:{line}: byte 0x{b:02x}", file.display()));
                break; // one report per file is enough
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "control bytes embedded in text sources:\n{}",
        offenders.join("\n")
    );
}
