use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::Result;
use serde_json::Value;
use tempfile::tempdir;
use walkdir::WalkDir;

fn messages(workspace: &Path, target: &Path, args: &[&str]) -> Result<Vec<Value>> {
    let output = Command::new(env!("CARGO"))
        .current_dir(workspace)
        .env("CARGO_TARGET_DIR", target)
        .args(args)
        .args(["--offline", "--message-format=json"])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "cargo {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with('{'))
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

fn output_paths(messages: &[Value]) -> BTreeSet<PathBuf> {
    messages
        .iter()
        .filter(|message| message["reason"] == "compiler-artifact")
        .flat_map(|message| message["filenames"].as_array().into_iter().flatten())
        .filter_map(Value::as_str)
        .map(PathBuf::from)
        .collect()
}

#[test]
fn cargo_json_does_not_inventory_intermediate_files() -> Result<()> {
    let temp = tempdir()?;
    let workspace = temp.path().join("workspace");
    let target = temp.path().join("target");
    let dep = workspace.join("dep");
    let app = workspace.join("app");
    fs::create_dir_all(dep.join("src"))?;
    fs::create_dir_all(app.join("src"))?;
    fs::create_dir_all(app.join("tests"))?;
    fs::create_dir_all(app.join("benches"))?;
    fs::write(
        workspace.join("Cargo.toml"),
        "[workspace]\nresolver = \"2\"\nmembers = [\"dep\", \"app\"]\n",
    )?;
    fs::write(
        dep.join("Cargo.toml"),
        "[package]\nname = \"dep\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    fs::write(dep.join("src/lib.rs"), "pub fn value() -> u8 { 42 }\n")?;
    fs::write(
        app.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n[dependencies]\ndep = { path = \"../dep\" }\n[features]\nextra = []\n[[bench]]\nname = \"smoke\"\nharness = false\n",
    )?;
    fs::write(
        app.join("src/lib.rs"),
        "pub fn value() -> u8 { dep::value() }\n",
    )?;
    fs::write(
        app.join("tests/smoke.rs"),
        "#[test] fn smoke() { assert_eq!(app::value(), 42); }\n",
    )?;
    fs::write(
        app.join("benches/smoke.rs"),
        "fn main() { assert_eq!(app::value(), 42); }\n",
    )?;
    fs::write(
        app.join("build.rs"),
        "fn main() { let out = std::env::var(\"OUT_DIR\").unwrap(); std::fs::write(std::path::Path::new(&out).join(\"generated.txt\"), \"ok\").unwrap(); }\n",
    )?;

    let host_output = Command::new("rustc").arg("-vV").output()?;
    anyhow::ensure!(host_output.status.success());
    let host_text = String::from_utf8(host_output.stdout)?;
    let host = host_text
        .lines()
        .find_map(|line| line.strip_prefix("host: "))
        .ok_or_else(|| anyhow::anyhow!("rustc -vV omitted host triple"))?;

    let commands = [
        vec!["build", "--workspace", "--features", "app/extra"],
        vec!["test", "--no-run", "--workspace", "--features", "app/extra"],
        vec![
            "bench",
            "--no-run",
            "--workspace",
            "--features",
            "app/extra",
        ],
        vec!["build", "--workspace", "--release"],
        vec!["build", "--workspace", "--target", host],
    ];
    let mut reported = BTreeSet::new();
    let mut build_script_dirs = BTreeSet::new();
    for args in commands {
        let result = messages(&workspace, &target, &args)?;
        let paths = output_paths(&result);
        anyhow::ensure!(!paths.is_empty(), "no artifact messages for {args:?}");
        reported.extend(paths);
        for message in &result {
            if message["reason"] == "build-script-executed" {
                if let Some(dir) = message["out_dir"].as_str() {
                    build_script_dirs.insert(PathBuf::from(dir));
                }
            }
        }
    }

    let fingerprints: Vec<_> = WalkDir::new(&target)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.file_type().is_file()
                && entry
                    .path()
                    .components()
                    .any(|part| part.as_os_str() == ".fingerprint")
        })
        .map(|entry| entry.into_path())
        .collect();
    anyhow::ensure!(!fingerprints.is_empty(), "Cargo produced no fingerprints");
    anyhow::ensure!(
        fingerprints.iter().all(|path| !reported.contains(path)),
        "compiler-artifact messages unexpectedly listed fingerprint files"
    );
    anyhow::ensure!(
        !build_script_dirs.is_empty(),
        "no build-script output directories"
    );
    for dir in &build_script_dirs {
        let generated = dir.join("generated.txt");
        anyhow::ensure!(generated.exists(), "missing {}", generated.display());
        anyhow::ensure!(
            !reported.contains(&generated),
            "compiler-artifact messages unexpectedly listed build-script output"
        );
    }
    eprintln!(
        "Cargo JSON: {} reported outputs, {} unreported fingerprint files, {} unreported build-script outputs",
        reported.len(),
        fingerprints.len(),
        build_script_dirs.len()
    );
    Ok(())
}
