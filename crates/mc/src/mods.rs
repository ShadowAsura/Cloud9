use crate::{Error, atomically_write, safe_component};
use cloud9_core::{Manifest, ModRole};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};
use zip::ZipArchive;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LockedMod {
    pub mod_id: String,
    pub version: String,
    pub sha256: String,
    pub source: Option<String>,
    pub client_only: bool,
}

struct Candidate {
    path: PathBuf,
    id: String,
    display_name: String,
    version: String,
}

fn candidates(directory: &Path) -> Result<Vec<Candidate>, Error> {
    let mut result = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
        {
            continue;
        }
        let mut archive = ZipArchive::new(File::open(&path)?)?;
        let file = match archive.by_name("mcmod.info") {
            Ok(file) => file,
            Err(zip::result::ZipError::FileNotFound) => continue,
            Err(error) => return Err(error.into()),
        };
        if file.size() > 1024 * 1024 {
            return Err(Error::Config(format!(
                "oversized mcmod.info in {}",
                path.display()
            )));
        }
        let metadata: Value = match serde_json::from_reader(file) {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "invalid mcmod.info");
                continue;
            }
        };
        let Some(list) = metadata
            .as_array()
            .or_else(|| metadata["modList"].as_array())
        else {
            continue;
        };
        for record in list {
            let Some(id) = record["modid"].as_str().filter(|id| safe_component(id)) else {
                continue;
            };
            let Some(version) = record["version"]
                .as_str()
                .filter(|version| !version.is_empty())
            else {
                continue;
            };
            result.push(Candidate {
                path: path.clone(),
                id: id.to_owned(),
                version: version.to_owned(),
                display_name: record["name"].as_str().unwrap_or(id).to_owned(),
            });
        }
    }
    Ok(result)
}

fn version_matches(version: &str, range: Option<&str>) -> bool {
    let Some(range) = range.filter(|range| *range != "*") else {
        return true;
    };
    let (Some(left), Some(right)) = (range.chars().next(), range.chars().last()) else {
        return false;
    };
    if !matches!(left, '[' | '(') || !matches!(right, ']' | ')') {
        return range == version;
    }
    let inner = &range[1..range.len() - 1];
    let Some((minimum, maximum)) = inner.split_once(',') else {
        return left == '[' && right == ']' && inner == version;
    };
    let parse = |value: &str| {
        value
            .trim()
            .split('.')
            .map(str::parse::<u32>)
            .collect::<Result<Vec<_>, _>>()
    };
    let Ok(version) = parse(version) else {
        return false;
    };
    let compare = |bound: &str| {
        let mut bound = parse(bound).ok()?;
        let mut version = version.clone();
        let len = bound.len().max(version.len());
        bound.resize(len, 0);
        version.resize(len, 0);
        Some(version.cmp(&bound))
    };

    use std::cmp::Ordering::*;
    (minimum.trim().is_empty()
        || compare(minimum).is_some_and(|o| o == Greater || o == Equal && left == '['))
        && (maximum.trim().is_empty()
            || compare(maximum).is_some_and(|o| o == Less || o == Equal && right == ']'))
}

fn sha256(path: &Path) -> Result<String, Error> {
    let mut sha = Sha256::new();
    io::copy(&mut File::open(path)?, &mut sha)?;
    Ok(format!("{:x}", sha.finalize()))
}

pub fn resolve_mods(
    manifest: &Manifest,
    supplied: &Path,
    cache: &Path,
    server: bool,
) -> Result<Vec<LockedMod>, Error> {
    let selected: Vec<_> = manifest
        .mods
        .iter()
        .filter(|record| !server || !record.client_only)
        .collect();
    if !manifest.allow_multiple_lucky
        && selected
            .iter()
            .filter(|record| record.role == ModRole::Lucky)
            .count()
            > 1
    {
        return Err(Error::Config(
            "multiple Lucky Block addons require explicit manifest approval".into(),
        ));
    }
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let candidates = candidates(supplied)?;
    let mut resolved = Vec::new();
    for record in selected {
        let range = record
            .version_range
            .as_ref()
            .and_then(|value| value.value.as_deref());
        let expected = record
            .file_hint
            .as_ref()
            .and_then(|hint| hint.sha256.as_deref());
        let matches = candidates.iter().filter(|candidate| {
            record.mod_id.value.as_ref().map_or_else(
                || {
                    record
                        .aliases
                        .iter()
                        .chain([&record.display_name.value])
                        .any(|alias| alias.eq_ignore_ascii_case(&candidate.display_name))
                },
                |id| id.eq_ignore_ascii_case(&candidate.id),
            ) && version_matches(&candidate.version, range)
        });
        let mut verified = Vec::new();
        for candidate in matches {
            let hash = sha256(&candidate.path)?;
            if expected.is_none_or(|expected| expected.eq_ignore_ascii_case(&hash))
                && !verified.iter().any(|(_, previous)| previous == &hash)
            {
                verified.push((candidate, hash));
            }
        }
        let (candidate, hash) = match verified.as_slice() {
            [] if record.optional => continue,
            [] => {
                return Err(Error::Config(format!(
                    "Locate jar: no matching verified {} jar",
                    record.display_name.value
                )));
            }
            [entry] => entry,
            _ => {
                return Err(Error::Config(format!(
                    "Edit manifest: {} matches several local jars",
                    record.display_name.value
                )));
            }
        };
        if resolved.iter().any(|entry: &LockedMod| {
            entry.mod_id.eq_ignore_ascii_case(&candidate.id) && entry.sha256 != *hash
        }) {
            return Err(Error::Config(format!(
                "multiple versions of {} selected",
                candidate.id
            )));
        }
        let path = cache.join(format!("{hash}.jar"));
        if !path.is_file() || sha256(&path)? != *hash {
            let bytes = fs::read(&candidate.path)?;
            if format!("{:x}", Sha256::digest(&bytes)) != *hash {
                return Err(Error::Hash(candidate.path.clone()));
            }
            atomically_write(&path, &bytes)?;
        }
        resolved.push(LockedMod {
            mod_id: candidate.id.clone(),
            version: candidate.version.clone(),
            sha256: hash.clone(),
            source: record.file_hint.as_ref().and_then(|hint| hint.url.clone()),
            client_only: record.client_only,
        });
    }
    resolved.sort_by(|left, right| left.mod_id.cmp(&right.mod_id));
    resolved.dedup_by(|left, right| left.mod_id == right.mod_id && left.sha256 == right.sha256);
    Ok(resolved)
}

pub fn stage_mods(mods: &[LockedMod], cache: &Path, destination: &Path) -> Result<(), Error> {
    fs::create_dir_all(destination)?;
    for record in mods {
        let name = jar_name(record)?;
        let source = cache.join(&name);
        if sha256(&source)? != record.sha256 {
            return Err(Error::Hash(source));
        }
        let target = destination.join(name);
        // Copy so modifications to an instance cannot corrupt the shared cache.
        fs::copy(source, target)?;
    }
    verify_mods(mods, destination)
}

pub fn verify_mods(mods: &[LockedMod], destination: &Path) -> Result<(), Error> {
    let expected: std::collections::HashSet<_> =
        mods.iter().map(jar_name).collect::<Result<_, _>>()?;
    for entry in fs::read_dir(destination)? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
            && !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| expected.contains(name))
        {
            return Err(Error::Config(format!(
                "unexpected jar in instance: {}",
                path.display()
            )));
        }
    }
    for record in mods {
        let path = destination.join(jar_name(record)?);
        if !path.is_file() || sha256(&path)? != record.sha256 {
            return Err(Error::Hash(path));
        }
    }
    Ok(())
}

fn jar_name(record: &LockedMod) -> Result<String, Error> {
    if record.sha256.len() == 64 && record.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(format!("{}.jar", record.sha256))
    } else {
        Err(Error::Config("invalid mod lockfile hash".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn renamed_jar_matches_metadata_and_hash() -> Result<(), Box<dyn std::error::Error>> {
        let root = tempfile::tempdir()?;
        let supplied = root.path().join("supplied");
        fs::create_dir(&supplied)?;
        let file = File::create(supplied.join("untitled.jar"))?;
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("mcmod.info", zip::write::SimpleFileOptions::default())?;
        zip.write_all(br#"[{"modid":"spiral","version":"1.2","name":"Spiral Lucky Block"}]"#)?;
        zip.finish()?;
        let doc = cloud9_core::Document::from_value(serde_json::from_str(include_str!(
            "../../../schema/fixtures/v1/fixture-01.json"
        ))?)?;
        let cache = root.path().join("cache");
        let mods = resolve_mods(doc.manifest(), &supplied, &cache, false)?;
        assert_eq!(mods[0].mod_id, "spiral");
        assert!(version_matches("1.2.0", Some("[1.2,2)")));
        assert!(!version_matches("2", Some("[1.2,2)")));
        let stage = root.path().join("instance");
        stage_mods(&mods, &cache, &stage)?;
        fs::write(stage.join(jar_name(&mods[0])?), [])?;
        assert!(verify_mods(&mods, &stage).is_err());
        assert!(cache.join(jar_name(&mods[0])?).is_file());
        Ok(())
    }
}
