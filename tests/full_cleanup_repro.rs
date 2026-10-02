use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
    process::Command,
};

use anyhow::Result;
use fs_extra::dir::get_size;
use tempfile::tempdir;

fn run(command: &mut Command) -> Result<String> {
    let output = command.output()?;
    anyhow::ensure!(
        output.status.success(),
        "command failed: {command:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn sweep(project: &Path, target: &Path, args: &[&str]) -> Result<String> {
    run(Command::new(env!("CARGO_BIN_EXE_cargo-sweep"))
        .current_dir(project)
        .env("CARGO_TARGET_DIR", target)
        .args(["sweep", "--verbose"])
        .args(args))
}

#[test]
fn no_fingerprints_all_time_zero_and_cargo_clean() -> Result<()> {
    let temp = tempdir()?;
    let project = temp.path().join("project");
    let target = temp.path().join("target");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"full-clean-repro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(project.join("src/lib.rs"), "pub fn value() -> u8 { 42 }\n")?;
    run(Command::new(env!("CARGO"))
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", &target)
        .env("CARGO_INCREMENTAL", "1")
        .args(["build", "--offline"]))?;

    let fingerprint = target.join("debug/.fingerprint");
    anyhow::ensure!(fingerprint.exists());
    fs::remove_dir_all(&fingerprint)?;
    let incremental = target.join("debug/incremental");
    anyhow::ensure!(incremental.exists());
    let unknown = target.join("unrelated.txt");
    fs::write(&unknown, "preserve this until ownership is known")?;

    let before = get_size(&target)?;
    let time_preview = sweep(&project, &target, &["--time", "0", "--dry-run"])?;
    anyhow::ensure!(get_size(&target)? == before);
    let time_actual = sweep(&project, &target, &["--time", "0"])?;
    anyhow::ensure!(incremental.exists() && unknown.exists());
    let after_time = get_size(&target)?;

    let all_preview = sweep(&project, &target, &["--all", "--dry-run"])?;
    anyhow::ensure!(get_size(&target)? == after_time);
    let all_actual = sweep(&project, &target, &["--all"])?;
    anyhow::ensure!(unknown.exists());
    let after_all = get_size(&target)?;
    anyhow::ensure!(after_all < after_time, "--all removed no Cargo cache data");
    anyhow::ensure!(!incremental.exists(), "--all kept incremental data");
    anyhow::ensure!(all_preview.contains("Would remove:"));
    anyhow::ensure!(all_actual.contains("Successfully removed:"));
    let planned: BTreeMap<String, u64> = all_preview
        .lines()
        .filter_map(|line| line.split_once("Would remove: ").map(|(_, rest)| rest))
        .map(|rest| {
            let (path, size) = rest.rsplit_once(" (").unwrap();
            let bytes = size.strip_suffix(" bytes)").unwrap().parse().unwrap();
            (path.to_owned(), bytes)
        })
        .collect();
    let removed: BTreeSet<String> = all_actual
        .lines()
        .filter_map(|line| {
            line.split_once("Successfully removed: ")
                .map(|(_, path)| path.to_owned())
        })
        .collect();
    anyhow::ensure!(planned.keys().cloned().collect::<BTreeSet<_>>() == removed);
    anyhow::ensure!(planned.values().sum::<u64>() == after_time - after_all);
    let second_all = sweep(&project, &target, &["--all"])?;
    anyhow::ensure!(get_size(&target)? == after_all);
    anyhow::ensure!(second_all.contains("Cleaned: nothing"));

    run(Command::new(env!("CARGO"))
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", &target)
        .args(["clean", "--offline"]))?;
    let after_cargo_clean = if target.exists() {
        get_size(&target)?
    } else {
        0
    };
    eprintln!(
        "without fingerprints: before={before} after_time={after_time} after_all={after_all} after_cargo_clean={after_cargo_clean}"
    );
    eprintln!("--time 0 dry run:\n{time_preview}--time 0 actual:\n{time_actual}");
    eprintln!("--all dry run:\n{all_preview}--all actual:\n{all_actual}");
    Ok(())
}

#[test]
fn recursive_shared_target_preserves_unknown_paths() -> Result<()> {
    let temp = tempdir()?;
    let root = temp.path().join("projects");
    let target = temp.path().join("shared-target");
    let rustc = Command::new("rustc").arg("-vV").output()?;
    anyhow::ensure!(rustc.status.success());
    let version = String::from_utf8(rustc.stdout)?;
    let host = version
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| anyhow::anyhow!("rustc -vV omitted host triple"))?;
    fs::create_dir_all(root.join("nested"))?;
    fs::write(
        root.join("nested/Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"second\"]\n",
    )?;
    for name in ["first", "nested/second"] {
        let project = root.join(name);
        fs::create_dir_all(project.join("src"))?;
        fs::write(
            project.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
                name.replace('/', "-")
            ),
        )?;
        fs::write(project.join("src/lib.rs"), "pub fn value() -> u8 { 42 }\n")?;
        let mut build = Command::new(env!("CARGO"));
        build
            .current_dir(&project)
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_INCREMENTAL", "1")
            .args(["build", "--offline"]);
        if name == "first" {
            build.args(["--target", host]);
        }
        run(&mut build)?;
    }
    let nested_deps = target.join(host).join("debug/deps");
    anyhow::ensure!(nested_deps.exists());

    let unknown_root = target.join("unrelated.txt");
    let unknown_profile = target.join("debug/unrelated.txt");
    fs::write(&unknown_root, "keep")?;
    fs::write(&unknown_profile, "keep")?;
    let outside = temp.path().join("outside.txt");
    fs::write(&outside, "keep")?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, target.join("debug/native"))?;

    let before = get_size(&target)?;
    let preview = sweep(&root, &target, &["--recursive", "--all", "--dry-run"])?;
    anyhow::ensure!(
        get_size(&target)? == before,
        "dry run changed shared target"
    );
    anyhow::ensure!(preview.matches("Would clean:").count() == 1);
    let actual = sweep(&root, &target, &["--recursive", "--all"])?;
    anyhow::ensure!(actual.matches("Cleaned:").count() == 1);
    anyhow::ensure!(get_size(&target)? < before);
    anyhow::ensure!(!nested_deps.exists());
    anyhow::ensure!(unknown_root.exists() && unknown_profile.exists() && outside.exists());
    #[cfg(unix)]
    anyhow::ensure!(target.join("debug/native").is_symlink());
    let second = sweep(&root, &target, &["--recursive", "--all"])?;
    anyhow::ensure!(second.contains("Cleaned: nothing"));
    Ok(())
}

#[test]
fn duplicate_project_paths_select_target_once() -> Result<()> {
    let temp = tempdir()?;
    let project = temp.path().join("project");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"duplicate-project\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(project.join("src/lib.rs"), "pub fn value() -> u8 { 42 }\n")?;
    run(Command::new(env!("CARGO"))
        .current_dir(&project)
        .args(["build", "--offline"]))?;

    let output = run(Command::new(env!("CARGO_BIN_EXE_cargo-sweep"))
        .current_dir(&project)
        .args(["sweep", "--all", "-v", ".", "./"]))?;
    anyhow::ensure!(output.matches("Selected target:").count() == 1);
    Ok(())
}

#[test]
fn explicit_target_directory_works_without_manifest() -> Result<()> {
    let temp = tempdir()?;
    let target = temp.path().join("target");
    let unknown = target.join("unrelated.txt");
    fs::create_dir_all(target.join("debug/deps"))?;
    fs::write(target.join(".rustc_info.json"), "cargo target marker")?;
    fs::write(target.join("debug/deps/libowned.rlib"), "owned")?;
    fs::write(&unknown, "preserve")?;

    let preview = run(Command::new(env!("CARGO_BIN_EXE_cargo-sweep"))
        .current_dir(temp.path())
        .args([
            "sweep",
            "--all",
            "--target-dir",
            target.to_str().unwrap(),
            "--dry-run",
            "--verbose",
        ]))?;
    anyhow::ensure!(preview.contains("Would remove:"));
    anyhow::ensure!(unknown.exists());

    let actual = run(Command::new(env!("CARGO_BIN_EXE_cargo-sweep"))
        .current_dir(temp.path())
        .args([
            "sweep",
            "--all",
            "--target-dir",
            target.to_str().unwrap(),
            "--verbose",
        ]))?;
    anyhow::ensure!(actual.contains("Successfully removed:"));
    anyhow::ensure!(unknown.exists());
    anyhow::ensure!(!target.join("debug/deps/libowned.rlib").exists());

    let rejected = temp.path().join("rejected");
    fs::create_dir_all(rejected.join("debug/deps"))?;
    fs::write(rejected.join("debug/deps/fake.rlib"), "not Cargo-owned")?;
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-sweep"))
        .current_dir(temp.path())
        .args(["sweep", "--all", "--target-dir", rejected.to_str().unwrap()])
        .output()?;
    anyhow::ensure!(!output.status.success());
    anyhow::ensure!(rejected.join("debug/deps/fake.rlib").exists());

    let output = Command::new(env!("CARGO_BIN_EXE_cargo-sweep"))
        .current_dir(temp.path())
        .args([
            "sweep",
            "--all",
            "--target-dir",
            target.to_str().unwrap(),
            "some-project",
        ])
        .output()?;
    anyhow::ensure!(!output.status.success());
    anyhow::ensure!(String::from_utf8_lossy(&output.stderr)
        .contains("--target-dir cannot be combined with positional paths"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlinked_target_root_fails_without_deleting() -> Result<()> {
    let temp = tempdir()?;
    let project = temp.path().join("project");
    let target = temp.path().join("target");
    let link = temp.path().join("target-link");
    fs::create_dir_all(project.join("src"))?;
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"symlinked-target\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(project.join("src/lib.rs"), "pub fn value() -> u8 { 42 }\n")?;
    run(Command::new(env!("CARGO"))
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", &target)
        .args(["build", "--offline"]))?;
    std::os::unix::fs::symlink(&target, &link)?;
    let before = get_size(&target)?;
    let output = Command::new(env!("CARGO_BIN_EXE_cargo-sweep"))
        .current_dir(&project)
        .env("CARGO_TARGET_DIR", &link)
        .args(["sweep", "--all"])
        .output()?;
    anyhow::ensure!(!output.status.success());
    anyhow::ensure!(String::from_utf8_lossy(&output.stdout).contains("not a real directory"));
    anyhow::ensure!(get_size(&target)? == before);
    Ok(())
}
