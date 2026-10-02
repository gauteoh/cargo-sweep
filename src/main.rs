use anyhow::Context;
use cargo_metadata::{Error, Metadata, MetadataCommand};
use crossterm::tty::IsTty;
use fern::colors::{Color, ColoredLevelConfig};

use log::{debug, error, info, warn};
use std::{
    env,
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use walkdir::WalkDir;

mod cli;
mod fingerprint;
mod full_clean;
mod stamp;
mod util;

use self::cli::Criterion;
use self::fingerprint::{
    hash_toolchains, remove_not_built_with, remove_older_than, remove_older_until_fits,
};
use self::full_clean::remove_recognized_cargo_cache;
use self::stamp::Timestamp;
use self::util::{format_bytes, format_bytes_or_nothing};

/// Setup logging according to verbose flag.
fn setup_logging(verbosity_level: u8) {
    let level = match verbosity_level {
        0 => log::LevelFilter::Info,
        1 => log::LevelFilter::Debug,
        2.. => log::LevelFilter::Trace,
    };

    let isatty = std::io::stdout().is_tty();

    // Configure colors for each log line
    let level_colors = ColoredLevelConfig::new()
        .info(Color::Green)
        .error(Color::Red)
        .warn(Color::Yellow)
        .debug(Color::BrightBlack);

    fern::Dispatch::new()
        .format(move |out, message, record| {
            if isatty {
                let reset = "\x1B[0m";
                let level_color = level_colors.color(record.level());
                out.finish(format_args!("{reset}[{level_color}{reset}] {message}"));
            } else {
                out.finish(format_args!("[{}] {message}", record.level()));
            }
        })
        .level(level)
        .chain(std::io::stdout())
        .apply()
        .unwrap();
}

fn target_directory_is_owned(target_directory: &Path) -> bool {
    target_directory.join(".rustc_info.json").is_file()
}

fn target_directory_is_safe(project_path: &Path, target_directory: &Path) -> bool {
    if env::var_os("CARGO_TARGET_DIR").is_some() {
        return true;
    }

    let project_path =
        fs::canonicalize(project_path).unwrap_or_else(|_| project_path.to_path_buf());
    let target_directory =
        fs::canonicalize(target_directory).unwrap_or_else(|_| target_directory.to_path_buf());
    target_directory.starts_with(project_path)
}

/// Returns whether the given path to a Cargo.toml points to a real target directory.
fn is_cargo_root(path: &Path) -> (Option<PathBuf>, bool) {
    if let Ok(metadata) = metadata(path) {
        let out = Path::new(&metadata.target_directory).to_path_buf();
        let project_path = path.parent().unwrap_or(path);
        if out.exists() && target_directory_is_safe(project_path, &out) {
            return (Some(out), false);
        }
        if out.exists() {
            error!("Skipping target directory outside project: {out:?}");
            return (None, true);
        }
    }
    (None, false)
}

/// is a `DirEntry` a unix stile hidden file, ie starts with `.`
fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .is_some_and(|s| s.starts_with('.'))
}

/// Find all cargo project under the given root path.
fn find_cargo_projects(root: &Path, include_hidden: bool) -> (Vec<PathBuf>, bool) {
    let mut target_paths = std::collections::BTreeSet::new();
    let mut discovery_failed = false;

    let mut iter = WalkDir::new(root).min_depth(1).into_iter();

    while let Some(entry) = iter.next() {
        if let Ok(entry) = entry {
            if entry.file_type().is_dir() {
                if !include_hidden && is_hidden(&entry) {
                    debug!("skip hidden folder {}", entry.path().display());
                    iter.skip_current_dir();
                    continue;
                }
                if entry.path().ancestors().any(|a| target_paths.contains(a)) {
                    // no reason to look at the contents of something we are already cleaning.
                    // Yes ancestors is a inefficient way to check. We can use a trie or something if it is slow.
                    iter.skip_current_dir();
                    continue;
                }
            }
            if entry.file_name() != "Cargo.toml" {
                continue;
            }
            let (target_directory, failed) = is_cargo_root(entry.path());
            discovery_failed |= failed;
            if let Some(target_directory) = target_directory {
                target_paths.insert(target_directory);
                // Previously cargo-sweep skipped subdirectories here, but it is valid for
                // subdirectories to contain cargo roots.
            }
        }
    }
    (target_paths.into_iter().collect(), discovery_failed)
}

fn metadata(path: &Path) -> Result<Metadata, Error> {
    let manifest_path = if path.file_name().and_then(OsStr::to_str) == Some("Cargo.toml") {
        path.to_owned()
    } else {
        path.join("Cargo.toml")
    };

    MetadataCommand::new()
        .manifest_path(manifest_path)
        .no_deps()
        .exec()
}

fn main() -> anyhow::Result<()> {
    let args = cli::parse();

    let criterion = args.criterion()?;
    let dry_run = args.dry_run;
    setup_logging(args.verbose);

    // Default to current invocation path.
    let has_positional_paths = !args.path.is_empty();
    let paths = match args.path.len() {
        0 => vec![env::current_dir().expect("Failed to get current directory")],
        _ => args.path,
    };

    // FIXME: Change to write to every passed in path instead of just the first one
    if let Criterion::Stamp = criterion {
        if paths.len() > 1 {
            anyhow::bail!("Using multiple paths and --stamp is currently unsupported");
        }

        debug!("Writing timestamp file in: {:?}", paths[0]);
        return Timestamp::new()
            .store(paths[0].as_path())
            .context("Failed to write timestamp file");
    };

    let file_duration = if matches!(criterion, Criterion::File) {
        warn!("--file uses access times, which may be static or unavailable; artifacts needed by the next build can be removed");
        Some(Duration::from(Timestamp::load(
            paths[0].as_path(),
            dry_run,
        )?))
    } else {
        None
    };

    let mut discovery_failed = false;
    let processed_paths = if let Some(target_dir) = &args.target_dir {
        if has_positional_paths {
            anyhow::bail!("--target-dir cannot be combined with positional paths");
        }
        if !target_dir.exists() {
            anyhow::bail!("Target directory does not exist: {}", target_dir.display());
        }
        if !target_directory_is_owned(target_dir) {
            anyhow::bail!(
                "Refusing target directory without Cargo ownership markers: {}",
                target_dir.display()
            );
        }
        vec![target_dir.clone()]
    } else if args.recursive {
        info!("Searching recursively for Rust project folders");
        let mut target_paths = std::collections::BTreeSet::new();
        for path in &paths {
            let (found_paths, failed) = find_cargo_projects(path, args.hidden);
            discovery_failed |= failed;
            target_paths.extend(found_paths);
        }
        target_paths.into_iter().collect::<Vec<_>>()
    } else {
        let mut return_paths = std::collections::BTreeSet::new();
        for path in &paths {
            let metadata = match metadata(path).context(format!(
                "Failed to gather metadata for {:?}",
                path.display()
            )) {
                Ok(metadata) => metadata,
                Err(error) => {
                    error!("{error:#}");
                    discovery_failed = true;
                    continue;
                }
            };
            let out = Path::new(&metadata.target_directory).to_path_buf();
            if !out.exists() {
                error!("Failed to clean {:?} as it does not exist.", out);
                discovery_failed = true;
            } else if !target_directory_is_safe(path, &out) {
                error!("Refusing to clean target directory outside project: {out:?}");
                discovery_failed = true;
            } else {
                return_paths.insert(out);
            };
        }
        return_paths.into_iter().collect()
    };

    let mut total_cleaned = 0;
    let mut all_failed = discovery_failed;

    // `None`: do not remove based on toolchain version
    // `Some(None)`: remove all installed toolchains
    // `Some(Some(Vec))`: remove only the specified toolchains
    let toolchains = match &criterion {
        Criterion::Installed => Some(None),
        Criterion::Toolchains(vec) => Some(Some(vec.clone())),
        _ => None,
    };
    if let Some(toolchains) = toolchains {
        let hashed_toolchains = match hash_toolchains(toolchains.as_ref()) {
            Ok(toolchains) => toolchains,
            Err(err) => {
                error!("{:?}", err.context("Failed to load toolchains."));
                anyhow::bail!("Failed to load toolchains");
            }
        };

        for project_path in &processed_paths {
            match remove_not_built_with(project_path, &hashed_toolchains, dry_run) {
                Ok(cleaned_amount) => {
                    let action = if dry_run { "Would clean" } else { "Cleaned" };
                    info!(
                        "{action}: {} from {project_path:?}",
                        format_bytes_or_nothing(cleaned_amount)
                    );
                    total_cleaned += cleaned_amount;
                }
                Err(e) => {
                    all_failed = true;
                    error!(
                        "{:?}",
                        e.context(format!("Failed to clean {project_path:?}"))
                    );
                }
            };
        }
    } else if let Criterion::All = criterion {
        for project_path in &processed_paths {
            match remove_recognized_cargo_cache(project_path, dry_run) {
                Ok(report) => {
                    let action = if dry_run { "Would clean" } else { "Cleaned" };
                    info!(
                        "{action}: {} from {project_path:?}",
                        format_bytes_or_nothing(report.removed_bytes)
                    );
                    total_cleaned += report.removed_bytes;
                    for error in report.errors {
                        all_failed = true;
                        error!("{error:#}");
                    }
                }
                Err(e) => {
                    all_failed = true;
                    error!("Failed to clean {:?}: {:?}", project_path, e);
                }
            }
        }
    } else if let Criterion::MaxSize(size) = criterion {
        for project_path in &processed_paths {
            match remove_older_until_fits(project_path, size, dry_run) {
                Ok(cleaned_amount) => {
                    let action = if dry_run { "Would clean" } else { "Cleaned" };
                    info!(
                        "{action}: {} from {project_path:?}",
                        format_bytes_or_nothing(cleaned_amount)
                    );
                    total_cleaned += cleaned_amount;
                }
                Err(e) => {
                    all_failed = true;
                    error!("Failed to clean {:?}: {:?}", project_path, e);
                }
            };
        }
    } else {
        let keep_duration = if let Some(duration) = file_duration {
            duration
        } else if let Criterion::Time(days_to_keep) = criterion {
            Duration::from_secs(days_to_keep * 24 * 3600)
        } else {
            unreachable!("unknown criteria {:?}", criterion);
        };

        for project_path in &processed_paths {
            match remove_older_than(project_path, &keep_duration, dry_run) {
                Ok(cleaned_amount) => {
                    let action = if dry_run { "Would clean" } else { "Cleaned" };
                    info!(
                        "{action}: {} from {project_path:?}",
                        format_bytes_or_nothing(cleaned_amount)
                    );
                    total_cleaned += cleaned_amount;
                }
                Err(e) => {
                    all_failed = true;
                    error!("Failed to clean {:?}: {:?}", project_path, e);
                }
            };
        }
    }

    if processed_paths.len() > 1 {
        info!("Total amount: {}", format_bytes(total_cleaned));
    }

    if all_failed {
        anyhow::bail!("Cleanup failed for one or more target directories");
    }

    if matches!(criterion, Criterion::File) && !dry_run {
        Timestamp::remove(paths[0].as_path()).context("Failed to remove timestamp file")?;
    }

    Ok(())
}
