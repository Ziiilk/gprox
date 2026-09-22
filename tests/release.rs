#![cfg(windows)]

use serde_json::Value;
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn script(root: &Path, name: &str, args: &[&str]) -> Output {
    Command::new("pwsh")
        .args(["-NoProfile", "-File"])
        .arg(root.join("scripts").join(name))
        .args(args)
        .output()
        .unwrap()
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn git(root: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn version_scripts_keep_manifest_lock_and_tags_consistent() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("scripts")).unwrap();
    for name in [
        "version-common.ps1",
        "bump-version.ps1",
        "check-version.ps1",
        "tag-release.ps1",
    ] {
        fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("scripts")
                .join(name),
            root.join("scripts").join(name),
        )
        .unwrap();
    }
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"gprox\"\nversion = \"0.1.9\"\n\n[dependencies]\nexample = \"0.1.9\"\n",
    )
    .unwrap();
    fs::write(root.join("Cargo.lock"), "version = 4\n\n[[package]]\nname = \"example\"\nversion = \"0.1.9\"\n\n[[package]]\nname = \"gprox\"\nversion = \"0.1.9\"\n").unwrap();
    let original = fs::read(root.join("Cargo.toml")).unwrap();
    assert!(success(script(root, "bump-version.ps1", &["-DryRun"])).contains("0.1.9 -> 0.1.10"));
    assert_eq!(fs::read(root.join("Cargo.toml")).unwrap(), original);
    success(script(root, "bump-version.ps1", &[]));
    success(script(root, "check-version.ps1", &["-Tag", "v0.1.10"]));
    assert!(
        fs::read_to_string(root.join("Cargo.toml"))
            .unwrap()
            .contains("example = \"0.1.9\"")
    );
    assert!(
        fs::read_to_string(root.join("Cargo.lock"))
            .unwrap()
            .contains("name = \"example\"\nversion = \"0.1.9\"")
    );
    assert!(
        !script(root, "check-version.ps1", &["-Tag", "v0.1.9"])
            .status
            .success()
    );
    for version in ["0.1.9", "v0.2.0", "0.2.0-beta", "00.2.0"] {
        assert!(
            !script(root, "bump-version.ps1", &["-Version", version])
                .status
                .success()
        );
    }
    success(script(root, "bump-version.ps1", &["-Part", "minor"]));
    success(script(root, "check-version.ps1", &["-Tag", "v0.2.0"]));
    success(script(root, "bump-version.ps1", &["-Part", "major"]));
    success(script(root, "check-version.ps1", &["-Tag", "v1.0.0"]));
    success(git(root, &["init", "--quiet"]));
    success(git(root, &["config", "user.name", "Release Test"]));
    success(git(
        root,
        &["config", "user.email", "release-test@example.invalid"],
    ));
    assert!(
        !script(root, "tag-release.ps1", &["-DryRun"])
            .status
            .success()
    );
    success(git(root, &["add", "."]));
    success(git(root, &["commit", "--quiet", "-m", "test fixture"]));
    success(script(root, "tag-release.ps1", &["-DryRun"]));
    assert!(success(git(root, &["tag", "--list"])).trim().is_empty());
    success(script(root, "tag-release.ps1", &[]));
    assert_eq!(
        success(git(root, &["cat-file", "-t", "v1.0.0"])).trim(),
        "commit"
    );
    assert!(!script(root, "tag-release.ps1", &[]).status.success());
    fs::write(
        root.join("Cargo.lock"),
        "version = 4\n[[package]]\nname = \"gprox\"\nversion = \"9.9.9\"\n",
    )
    .unwrap();
    assert!(!script(root, "check-version.ps1", &[]).status.success());
}

#[test]
fn version_command_has_no_state_directory_side_effect() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("unused-home");
    let output = Command::new(env!("CARGO_BIN_EXE_gprox"))
        .arg("--home")
        .arg(&home)
        .args(["version", "--json"])
        .output()
        .unwrap();
    let json: Value = serde_json::from_str(&success(output)).unwrap();
    assert_eq!(json["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(json["repository"], "https://github.com/Ziiilk/gprox");
    assert!(!home.exists());
}
