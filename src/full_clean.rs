use anyhow::Result;
use log::{debug, info};
use std::{
    fs,
    path::{Path, PathBuf},
};
use walkdir::WalkDir;

const CACHE_DIRS: [&str; 6] = [
    ".fingerprint",
    "build",
    "deps",
    "incremental",
    "examples",
    "native",
];

struct Candidate {
    path: PathBuf,
    bytes: u64,
}

struct Inventory {
    candidates: Vec<Candidate>,
    skipped: Vec<PathBuf>,
}

/// Record confirmed removals and errors for individual cache directories.
pub struct CleanReport {
    pub removed_bytes: u64,
    pub errors: Vec<anyhow::Error>,
}

fn is_real_dir(path: &Path) -> Result<bool> {
    Ok(fs::symlink_metadata(path)?.file_type().is_dir())
}

fn has_cache_dir(path: &Path) -> Result<bool> {
    for name in CACHE_DIRS {
        let child = path.join(name);
        if child.exists() && is_real_dir(&child)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_target_triple(name: &str) -> bool {
    name.matches('-').count() >= 2
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-_.".contains(character))
}

fn profile_paths(target: &Path) -> Result<Vec<PathBuf>> {
    let mut profiles = Vec::new();
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        let path = entry.path();
        if !entry.file_type()?.is_dir() {
            continue;
        }
        if has_cache_dir(&path)? {
            profiles.push(path);
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if is_target_triple(name) {
            for nested in fs::read_dir(&path)? {
                let nested = nested?;
                if nested.file_type()?.is_dir() && has_cache_dir(&nested.path())? {
                    profiles.push(nested.path());
                }
            }
        }
    }
    Ok(profiles)
}

fn directory_bytes(path: &Path) -> Result<u64> {
    let mut bytes = 0;
    for entry in WalkDir::new(path).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file() {
            bytes += entry.metadata()?.len();
        }
    }
    Ok(bytes)
}

fn inventory(target: &Path) -> Result<Inventory> {
    anyhow::ensure!(
        is_real_dir(target)?,
        "target is not a real directory: {}",
        target.display()
    );
    let profiles = profile_paths(target)?;
    let mut candidates = Vec::new();
    let mut skipped = Vec::new();
    for profile in &profiles {
        for entry in fs::read_dir(profile)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let recognized = CACHE_DIRS.iter().any(|known| name == *known);
            if recognized && entry.file_type()?.is_dir() {
                candidates.push(Candidate {
                    bytes: directory_bytes(&path)?,
                    path,
                });
            } else {
                skipped.push(path);
            }
        }
    }
    for entry in fs::read_dir(target)? {
        let entry = entry?;
        let path = entry.path();
        if !profiles
            .iter()
            .any(|profile| profile == &path || profile.starts_with(&path))
        {
            skipped.push(path);
        }
    }
    candidates.sort_by(|a, b| a.path.cmp(&b.path));
    skipped.sort();
    Ok(Inventory {
        candidates,
        skipped,
    })
}

/// Remove recognized Cargo cache directories from a verified target path.
/// An inventory error stops cleanup before any deletion begins.
pub fn remove_recognized_cargo_cache(target: &Path, dry_run: bool) -> Result<CleanReport> {
    let inventory = inventory(target)?;
    info!(
        "{} recognized Cargo cache directories in {:?}",
        if dry_run { "Would remove" } else { "Removing" },
        target
    );
    info!(
        "Skipped {} unrecognized paths; use --verbose to list them",
        inventory.skipped.len()
    );
    for path in &inventory.skipped {
        debug!("Skipped unrecognized path: {:?}", path);
    }
    let mut report = CleanReport {
        removed_bytes: 0,
        errors: Vec::new(),
    };
    for candidate in inventory.candidates {
        if dry_run {
            info!(
                "Would remove: {:?} ({} bytes)",
                candidate.path, candidate.bytes
            );
            report.removed_bytes += candidate.bytes;
        } else {
            match fs::remove_dir_all(&candidate.path) {
                Ok(()) => {
                    info!("Successfully removed: {:?}", candidate.path);
                    report.removed_bytes += candidate.bytes;
                }
                Err(error) => report.errors.push(
                    anyhow::Error::new(error)
                        .context(format!("Failed to remove {}", candidate.path.display())),
                ),
            }
        }
    }
    Ok(report)
}
