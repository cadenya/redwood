//! Output lifecycle: writing a generation safely into a directory redwood
//! does not exclusively own.
//!
//! A `.redwood-manifest.json` in the output directory records exactly which
//! relative paths the previous generation emitted. On the next run, paths
//! that are no longer generated are removed (and only those — user files
//! next to generated ones are never touched), then the manifest is replaced.
//! Formatting likewise applies only to generated files, never the directory.
//!
//! Ordering matters: everything is validated before the first mutation, and
//! stale paths are removed BEFORE new files are written so layout migrations
//! (a file becoming a directory, or the reverse) succeed. Individual file
//! replacement is not atomic; the manifest itself is written via temp+rename
//! so a crash never records a half-written ownership list.
//!
//! Confinement assumption: the output directory is not modified CONCURRENTLY
//! by an adversary — symlink checks are check-then-act and cannot beat a
//! race. They defend against pre-existing layout mistakes, not a live
//! attacker with write access to the tree.

use std::collections::BTreeSet;
use std::path::{Component, Path};

use anyhow::{bail, Context, Result};

use crate::backends::FileSet;

const MANIFEST_FILE: &str = ".redwood-manifest.json";

/// Write a generation: remove paths owned by the previous generation that
/// this one no longer produces, emit every file, and record ownership.
/// `format_go` runs gofmt over the generated .go files (and only those).
pub fn write(out_dir: &Path, files: &FileSet, format_go: bool) -> Result<()> {
    // The manifest is untrusted input that DRIVES deletion, so its own
    // integrity is the first check: a symlinked manifest could smuggle in a
    // path list claiming ordinary user files as stale.
    for name in [MANIFEST_FILE.to_string(), format!("{MANIFEST_FILE}.tmp")] {
        let path = out_dir.join(&name);
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if !meta.is_file() {
                bail!(
                    "{} is not a regular file — refusing to trust or replace it",
                    path.display()
                );
            }
        }
    }
    let previous = read_manifest(out_dir)?;
    let current: BTreeSet<String> = files.keys().cloned().collect();

    // Validate every involved path before touching anything. A corrupt
    // manifest is an error, not a skip: silently ignoring entries would
    // leave an ownership record the next run wrongly trusts.
    for rel in current.iter().chain(previous.iter()) {
        validate_rel_path(rel)
            .with_context(|| format!("in {} or the generation", MANIFEST_FILE))?;
    }

    // Remove stale files first so layout migrations work in both directions
    // (previous file `x` -> current `x/y.go`, and previous `x/y.go` ->
    // current file `x` once the emptied directory is pruned).
    for stale in previous.difference(&current) {
        let path = out_dir.join(stale);
        refuse_symlinked_parents(out_dir, stale)?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_file() => {
                std::fs::remove_file(&path)
                    .with_context(|| format!("removing stale {}", path.display()))?;
                prune_empty_parents(out_dir, &path);
            }
            // Already gone, or something unexpected (dir/symlink) now lives
            // there — never delete what we can't positively identify as the
            // regular file we wrote.
            _ => {}
        }
    }

    for (rel_path, contents) in files {
        refuse_symlinked_parents(out_dir, rel_path)?;
        let path = out_dir.join(rel_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents).with_context(|| format!("writing {}", path.display()))?;
    }

    if format_go {
        gofmt_files(out_dir, &current)?;
        // A generated Go module must build cleanly with no consumer-side
        // `go mod tidy`: the emitted go.mod carries the full require set,
        // and this step materializes a matching go.sum from the module
        // cache/network so `go build ./...` works immediately.
        if current.contains("go.mod") {
            go_mod_tidy(out_dir)?;
        }
    }

    // temp + rename: the ownership record is either the old one or the new
    // one, never a torn write. (The manifest paths were integrity-checked
    // before anything was read or deleted.)
    let manifest = serde_json::to_string_pretty(&current.iter().collect::<Vec<_>>())?;
    let tmp = out_dir.join(format!("{MANIFEST_FILE}.tmp"));
    std::fs::write(&tmp, manifest).with_context(|| format!("writing {MANIFEST_FILE}"))?;
    std::fs::rename(&tmp, out_dir.join(MANIFEST_FILE))
        .with_context(|| format!("replacing {MANIFEST_FILE}"))?;
    Ok(())
}

fn read_manifest(out_dir: &Path) -> Result<BTreeSet<String>> {
    let path = out_dir.join(MANIFEST_FILE);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(BTreeSet::new());
    };
    let paths: Vec<String> = serde_json::from_str(&raw)
        .with_context(|| format!("parsing {} — delete it to reset ownership", path.display()))?;
    Ok(paths.into_iter().collect())
}

/// Generated paths must stay inside the output directory.
fn validate_rel_path(rel: &str) -> Result<()> {
    let path = Path::new(rel);
    if path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("generated path escapes the output directory: {rel}");
    }
    Ok(())
}

/// Lexical validation can't stop a symlinked path from redirecting a write
/// or delete outside the output tree; refuse any symlink among the path's
/// existing components — parents AND the final component (std::fs::write
/// follows a symlinked file).
fn refuse_symlinked_parents(out_dir: &Path, rel: &str) -> Result<()> {
    let mut current = out_dir.to_path_buf();
    for component in Path::new(rel).components() {
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                bail!(
                    "refusing to traverse symlink {} while handling generated path {rel}",
                    current.display()
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// Remove now-empty directories left behind by a stale file, up to (not
/// including) the output root.
fn prune_empty_parents(out_dir: &Path, removed: &Path) {
    let mut dir = removed.parent();
    while let Some(d) = dir {
        if d == out_dir {
            break;
        }
        // remove_dir fails on non-empty directories; that's the stop signal.
        if std::fs::remove_dir(d).is_err() {
            break;
        }
        dir = d.parent();
    }
}

/// gofmt exactly the generated .go files, in bounded batches so a very large
/// API can't overflow argv limits. Generated Go must be canonical, so a
/// missing gofmt is an error for Go targets, not a warning.
/// `go mod tidy` in the output module so go.sum is complete at generation
/// time. Hard error like gofmt: shipping a module that fails its first
/// `go build` is a generator defect, not a consumer chore.
fn go_mod_tidy(out_dir: &Path) -> Result<()> {
    match std::process::Command::new("go")
        .args(["mod", "tidy"])
        .current_dir(out_dir)
        .status()
    {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!("go mod tidy failed with {status} in {}", out_dir.display()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            bail!(
                "go not found: generating a Go target requires the Go toolchain \
                 so the emitted module builds cleanly (go.sum)"
            )
        }
        Err(err) => Err(err.into()),
    }
}

fn gofmt_files(out_dir: &Path, generated: &BTreeSet<String>) -> Result<()> {
    let go_files: Vec<&String> = generated.iter().filter(|p| p.ends_with(".go")).collect();
    for chunk in go_files.chunks(64) {
        let mut cmd = std::process::Command::new("gofmt");
        cmd.arg("-w").current_dir(out_dir);
        for f in chunk {
            cmd.arg(f);
        }
        match cmd.status() {
            Ok(status) if status.success() => {}
            Ok(status) => bail!("gofmt failed with {status} in {}", out_dir.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                bail!(
                    "gofmt not found: generating a Go target requires the Go toolchain \
                     so output is canonically formatted"
                )
            }
            Err(err) => return Err(err).context("running gofmt"),
        }
    }
    Ok(())
}
