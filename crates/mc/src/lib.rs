mod artifact;
mod auth;
mod client;
mod mods;
mod process;
mod registry;

pub use artifact::Installer;
pub use auth::{Account, MicrosoftSignIn};
pub use client::{
    ClientRuntime, Instance, create_instance, create_instance_with_helper, item_count, mod_set_hash,
};
pub use mods::{LockedMod, helper_mod, resolve_mods, stage_mods, verify_mods};
pub use process::{Diagnostic, Level, LogEvent, capture_logs, diagnose};
pub use registry::{Alias, Registry, Resolution};

use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Config(String),
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("HTTP: {0}")]
    Http(#[from] reqwest::Error),
    #[error("hash mismatch for {0}")]
    Hash(PathBuf),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Profile {
    pub mc_version: String,
    pub loader: String,
    pub loader_version: String,
    pub java_major: u8,
    pub forge_coordinate: String,
    pub forge_version_id: String,
    pub installer_sha1: String,
    pub universal_sha1: String,
}

impl Profile {
    pub fn primary() -> Result<Self, Error> {
        Ok(serde_json::from_str(include_str!(
            "../../../schema/profiles/default.json"
        ))?)
    }

    pub fn check_manifest(&self, manifest: &cloud9_core::Manifest) -> Result<(), Error> {
        let game = &manifest.game;
        if self.mc_version != game.mc_version.value
            || self.loader != game.loader.kind.value
            || Some(&self.loader_version) != game.loader.version.value.as_ref()
            || Some(u32::from(self.java_major)) != game.java_major.value
        {
            return Err(Error::Config(
                "the episode does not match the installed version profile".into(),
            ));
        }
        Ok(())
    }
}

pub fn java_major(path: &Path) -> Result<u8, Error> {
    let output = Command::new(path).arg("-version").output()?;
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let version = stderr
        .lines()
        .chain(stdout.lines())
        .find_map(|line| {
            let (_, version) = line.split_once("version \"")?;
            version.split('"').next()
        })
        .and_then(|version| {
            version
                .strip_prefix("1.")
                .unwrap_or(version)
                .split(['.', '-'])
                .next()
        })
        .and_then(|major| major.parse::<u8>().ok());
    match (output.status.success(), version) {
        (true, Some(major)) => Ok(major),
        _ => Err(Error::Config(format!(
            "could not determine Java version at {}",
            path.display()
        ))),
    }
}

pub fn discover_java(profile: &Profile, explicit: Option<&Path>) -> Result<PathBuf, Error> {
    if let Some(path) = explicit {
        let path = dunce::canonicalize(path)?;
        let major = java_major(&path)?;
        if major != profile.java_major {
            return Err(Error::Config(format!(
                "Java {} required, found Java {major}",
                profile.java_major
            )));
        }
        return Ok(path);
    }
    let executable = if cfg!(windows) { "java.exe" } else { "java" };
    let mut candidates = Vec::new();
    for variable in [
        format!("JAVA{}_HOME", profile.java_major),
        "JAVA_HOME".into(),
    ] {
        if let Some(path) = std::env::var_os(variable) {
            candidates.push(PathBuf::from(path).join("bin").join(executable));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|path| path.join(executable)));
    }
    for root in ["ProgramFiles", "ProgramFiles(x86)"]
        .into_iter()
        .filter_map(std::env::var_os)
    {
        for vendor in [
            "Java",
            "Eclipse Adoptium",
            "Microsoft",
            "Amazon Corretto",
            "Zulu",
        ] {
            if let Ok(entries) = fs::read_dir(PathBuf::from(&root).join(vendor)) {
                candidates.extend(
                    entries
                        .flatten()
                        .map(|entry| entry.path().join("bin").join(executable)),
                );
            }
        }
    }
    if let Ok(entries) = fs::read_dir("/usr/lib/jvm") {
        candidates.extend(
            entries
                .flatten()
                .map(|entry| entry.path().join("bin").join(executable)),
        );
    }
    for candidate in candidates {
        if candidate.is_file()
            && java_major(&candidate).is_ok_and(|major| major == profile.java_major)
        {
            return Ok(dunce::canonicalize(candidate)?);
        }
    }
    Err(Error::Config(format!(
        "Java {} is required. Set JAVA{}_HOME or supply its executable.",
        profile.java_major, profile.java_major
    )))
}

pub(crate) fn atomically_write(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    use std::io::Write;
    let parent = path
        .parent()
        .ok_or_else(|| Error::Config("file has no parent".into()))?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(crate) fn safe_component(component: &str) -> bool {
    let stem = component
        .split('.')
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    !component.is_empty()
        && component.len() <= 128
        && component.as_bytes()[0].is_ascii_alphanumeric()
        && component
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
        && !component.ends_with('.')
        && !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
}
mod bootstrap;
pub use bootstrap::{await_registry, bootstrap_client_registry, read_registry};
mod server_runtime;
pub use server_runtime::ServerRuntime;
