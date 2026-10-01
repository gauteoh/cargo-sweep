use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

use anyhow::{Context, Result};
use tempfile::tempdir;
use walkdir::WalkDir;

fn run(command: &mut Command) -> Result<Output> {
    let output = command.output()?;
    anyhow::ensure!(
        output.status.success(),
        "command failed: {command:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

fn files(root: &Path) -> BTreeSet<PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
        .map(|entry| entry.path().strip_prefix(root).unwrap().to_owned())
        .collect()
}

fn selected_paths(output: &Output, prefix: &str) -> BTreeSet<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_once(prefix).map(|(_, path)| path.to_owned()))
        .collect()
}

fn build(workspace: &Path, target: &Path, release: bool) -> Result<(usize, usize)> {
    let mut command = Command::new(env!("CARGO"));
    command
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", target)
        .env("CARGO_INCREMENTAL", "1")
        .args(["build", "--offline", "--message-format=json"]);
    if release {
        command.arg("--release");
    }
    let output = run(&mut command)?;
    let mut fresh = 0;
    let mut rebuilt = 0;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let Ok(message) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if message["reason"] == "compiler-artifact" {
            if message["fresh"] == true {
                fresh += 1;
            } else {
                rebuilt += 1;
            }
        }
    }
    Ok((fresh, rebuilt))
}

#[test]
fn stamp_noop_build_cleanup_rebuild_matrix() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(&workspace)?;
    fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"dep\", \"app-a\", \"app-b\"]\n",
    )?;
    for name in ["dep", "app-a", "app-b"] {
        let crate_dir = workspace.join(name);
        fs::create_dir_all(crate_dir.join("src"))?;
        let dependencies = if name == "dep" {
            String::new()
        } else {
            "[dependencies]\ndep = { path = \"../dep\" }\n".to_owned()
        };
        fs::write(
            crate_dir.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n{dependencies}"
            ),
        )?;
        let source = if name == "dep" {
            "pub fn value() -> usize { 42 }\n"
        } else {
            "pub fn value() -> usize { dep::value() }\n"
        };
        fs::write(crate_dir.join("src/lib.rs"), source)?;
    }

    for release in [false, true] {
        let profile = if release { "release" } else { "debug" };
        let target = temp.path().join(format!("target-{profile}"));
        let (_, initial_rebuilt) = build(&workspace, &target, release)?;
        anyhow::ensure!(
            initial_rebuilt >= 3,
            "initial {profile} build was incomplete"
        );

        let mut stamp = Command::new(env!("CARGO_BIN_EXE_cargo-sweep"));
        stamp
            .args(["sweep", "--stamp"])
            .current_dir(&workspace)
            .env("CARGO_TARGET_DIR", &target);
        run(&mut stamp)?;

        let (noop_fresh, noop_rebuilt) = build(&workspace, &target, release)?;
        anyhow::ensure!(
            noop_fresh >= 3 && noop_rebuilt == 0,
            "{profile} build was not a no-op: fresh={noop_fresh}, rebuilt={noop_rebuilt}"
        );

        let before = files(&target);
        let mut dry_run = Command::new(env!("CARGO_BIN_EXE_cargo-sweep"));
        dry_run
            .args(["sweep", "--file", "--dry-run", "--verbose"])
            .current_dir(&workspace)
            .env("CARGO_TARGET_DIR", &target);
        let preview = run(&mut dry_run)?;
        anyhow::ensure!(files(&target) == before, "dry run changed {profile} target");

        let mut sweep = Command::new(env!("CARGO_BIN_EXE_cargo-sweep"));
        sweep
            .args(["sweep", "--file", "--verbose"])
            .current_dir(&workspace)
            .env("CARGO_TARGET_DIR", &target);
        let actual = run(&mut sweep)?;
        let selected = selected_paths(&preview, "Would remove: ");
        let removed = selected_paths(&actual, "Successfully removed: ");
        anyhow::ensure!(
            selected == removed,
            "dry run and cleanup differed for {profile}"
        );

        let after = files(&target);
        let deleted_files = before.difference(&after).count();
        let retained_incremental = after
            .iter()
            .filter(|path| {
                path.components()
                    .any(|part| part.as_os_str() == "incremental")
            })
            .count();
        let (fresh_after, rebuilt_after) = build(&workspace, &target, release)
            .with_context(|| format!("rebuilding {profile} after stamp cleanup"))?;
        eprintln!(
            "{profile}: selected={} deleted_files={deleted_files} retained_incremental={retained_incremental} fresh_after={fresh_after} rebuilt_after={rebuilt_after}",
            selected.len()
        );
        eprintln!("{profile} selected paths: {selected:#?}");
    }
    Ok(())
}
