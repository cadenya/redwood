//! Output lifecycle: stale-file removal, unowned-file preservation, and
//! ownership-manifest bookkeeping across repeated generations.

use std::collections::BTreeMap;

fn fileset(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn regeneration_removes_stale_owned_files_only() {
    let dir = std::env::temp_dir().join(format!("redwood-output-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // A user file that redwood must never touch.
    std::fs::write(dir.join("user-notes.md"), "mine").unwrap();

    let first = fileset(&[
        ("client.py", "v1"),
        ("resources/agents.py", "v1"),
        ("resources/tools.py", "v1"),
    ]);
    redwood::output::write(&dir, &first, false).unwrap();
    assert!(dir.join("resources/tools.py").is_file());

    // Second generation drops the tools resource.
    let second = fileset(&[("client.py", "v2"), ("resources/agents.py", "v2")]);
    redwood::output::write(&dir, &second, false).unwrap();

    assert!(
        !dir.join("resources/tools.py").exists(),
        "stale file removed"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("client.py")).unwrap(),
        "v2"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("user-notes.md")).unwrap(),
        "mine",
        "unowned files are preserved"
    );

    // Dropping the whole resources/ dir prunes the now-empty directory.
    let third = fileset(&[("client.py", "v3")]);
    redwood::output::write(&dir, &third, false).unwrap();
    assert!(!dir.join("resources").exists(), "empty owned dir pruned");
    assert!(dir.join("user-notes.md").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_to_directory_and_back_migrations_succeed() {
    let dir = std::env::temp_dir().join(format!("redwood-output-mig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // previous owns file `resource`; current owns `resource/client.go`.
    let v1 = fileset(&[("resource", "flat")]);
    redwood::output::write(&dir, &v1, false).unwrap();
    let v2 = fileset(&[("resource/client.go", "package x\n")]);
    redwood::output::write(&dir, &v2, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("resource/client.go")).unwrap(),
        "package x\n"
    );

    // ...and back: `resource/client.go` -> file `resource`.
    let v3 = fileset(&[("resource", "flat again")]);
    redwood::output::write(&dir, &v3, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.join("resource")).unwrap(),
        "flat again"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn symlinked_parents_are_refused() {
    let dir = std::env::temp_dir().join(format!("redwood-output-sym-{}", std::process::id()));
    let victim = std::env::temp_dir().join(format!("redwood-victim-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&victim);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("precious.txt"), "keep me").unwrap();
    std::os::unix::fs::symlink(&victim, dir.join("linked")).unwrap();

    // Writing through the symlink is refused...
    let gen = fileset(&[("linked/generated.txt", "nope")]);
    let err = redwood::output::write(&dir, &gen, false).unwrap_err();
    assert!(err.to_string().contains("symlink"), "{err}");
    // ...and the external tree is untouched.
    assert_eq!(
        std::fs::read_to_string(victim.join("precious.txt")).unwrap(),
        "keep me"
    );
    assert!(!victim.join("generated.txt").exists());

    // A symlink as the FINAL component must also be refused: std::fs::write
    // would follow it and overwrite the external file.
    std::os::unix::fs::symlink(victim.join("precious.txt"), dir.join("aliased.txt")).unwrap();
    let gen = fileset(&[("aliased.txt", "overwritten")]);
    let err = redwood::output::write(&dir, &gen, false).unwrap_err();
    assert!(err.to_string().contains("symlink"), "{err}");
    assert_eq!(
        std::fs::read_to_string(victim.join("precious.txt")).unwrap(),
        "keep me"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&victim);
}

#[cfg(unix)]
#[test]
fn symlinked_manifest_cannot_drive_deletion() {
    let dir = std::env::temp_dir().join(format!("redwood-output-manif-{}", std::process::id()));
    let external = std::env::temp_dir().join(format!("redwood-manif-ext-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&external);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(&external).unwrap();

    // A user file redwood never generated...
    std::fs::write(dir.join("user-notes.md"), "mine").unwrap();
    // ...and a symlinked manifest whose external target claims it as owned.
    std::fs::write(external.join("evil.json"), r#"["user-notes.md"]"#).unwrap();
    std::os::unix::fs::symlink(
        external.join("evil.json"),
        dir.join(".redwood-manifest.json"),
    )
    .unwrap();

    let gen = fileset(&[("client.py", "v1")]);
    let err = redwood::output::write(&dir, &gen, false).unwrap_err();
    assert!(err.to_string().contains("not a regular file"), "{err}");
    assert_eq!(
        std::fs::read_to_string(dir.join("user-notes.md")).unwrap(),
        "mine",
        "the claimed user file must survive"
    );

    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&external);
}

#[test]
fn manifest_paths_are_confined_to_the_output_dir() {
    let dir = std::env::temp_dir().join(format!("redwood-output-esc-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let escaping = fileset(&[("../escape.txt", "nope")]);
    let err = redwood::output::write(&dir, &escaping, false).unwrap_err();
    let chain = format!("{err:#}");
    assert!(chain.contains("escapes the output directory"), "{chain}");

    let _ = std::fs::remove_dir_all(&dir);
}
