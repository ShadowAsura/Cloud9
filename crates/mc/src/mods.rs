use crate::{Error, atomically_write, safe_component};
use cloud9_core::{FileHint, Manifest, Mod, ModRole};
use reqwest::{Url, blocking::Client, redirect::Policy};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read},
    path::{Path, PathBuf},
    time::Duration,
};
use zip::ZipArchive;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LockedMod {
    #[serde(default)]
    pub entry_id: String,
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
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut result = Vec::new();
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_file()
            && entry
                .path()
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("jar"))
        {
            read_metadata(&entry.path(), &mut result)?;
        }
    }
    Ok(result)
}

fn read_metadata(path: &Path, result: &mut Vec<Candidate>) -> Result<(), Error> {
    let mut archive = match ZipArchive::new(File::open(path)?) {
        Ok(archive) => archive,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "unreadable local mod archive");
            return Ok(());
        }
    };
    let file = match archive.by_name("mcmod.info") {
        Ok(file) => file,
        Err(zip::result::ZipError::FileNotFound) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if file.size() > 1024 * 1024 {
        tracing::warn!(path = %path.display(), "oversized mcmod.info");
        return Ok(());
    }
    let metadata: Value = match serde_json::from_reader(file) {
        Ok(metadata) => metadata,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "invalid mcmod.info");
            return Ok(());
        }
    };
    let Some(list) = metadata
        .as_array()
        .or_else(|| metadata["modList"].as_array())
    else {
        return Ok(());
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
            path: path.to_path_buf(),
            id: id.to_owned(),
            version: version.to_owned(),
            display_name: record["name"].as_str().unwrap_or(id).to_owned(),
        });
    }
    Ok(())
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

fn jar_filename(name: &str) -> bool {
    !name.contains(['/', '\\', ':', '%'])
        && !name.chars().any(char::is_control)
        && name.to_ascii_lowercase().ends_with(".jar")
}

fn official_mod_file(url: &Url) -> bool {
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port_or_known_default() != Some(443)
    {
        return false;
    }
    let segments: Vec<_> = url.path_segments().into_iter().flatten().collect();
    match (url.host_str(), segments.as_slice()) {
        (Some("cdn.modrinth.com"), ["data", project, "versions", version, file]) => {
            !project.is_empty()
                && !version.is_empty()
                && file.to_ascii_lowercase().ends_with(".jar")
        }
        (
            Some("edge.forgecdn.net" | "media.forgecdn.net" | "mediafilez.forgecdn.net"),
            ["files", major, minor, file],
        ) => {
            !major.is_empty()
                && major.bytes().all(|b| b.is_ascii_digit())
                && minor.len() == 3
                && minor.bytes().all(|b| b.is_ascii_digit())
                && file.to_ascii_lowercase().ends_with(".jar")
        }
        _ => false,
    }
}

fn mod_download(hint: &FileHint) -> Option<(Url, String)> {
    let hash = hint.sha256.as_ref()?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut url = Url::parse(hint.url.as_deref()?).ok()?;
    if !official_mod_file(&url) {
        let segments: Vec<_> = url.path_segments()?.collect();
        let ["minecraft", "mc-mods", _, "files", file] = segments.as_slice() else {
            return None;
        };
        let filename = hint.filename.as_deref().filter(|name| jar_filename(name))?;
        if !matches!(
            url.host_str(),
            Some("www.curseforge.com" | "curseforge.com")
        ) || url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.port_or_known_default() != Some(443)
        {
            return None;
        }
        let id: u64 = file.parse().ok()?;
        let major = (id / 1000).to_string();
        let minor = format!("{:03}", id % 1000);
        url = Url::parse("https://edge.forgecdn.net").ok()?;
        url.path_segments_mut()
            .ok()?
            .extend(["files", &major, &minor, filename]);
    }
    Some((url, hash.to_ascii_lowercase()))
}

pub fn has_mod_downloads(manifest: &Manifest) -> bool {
    manifest
        .mods
        .iter()
        .any(|record| record.file_hint.as_ref().and_then(mod_download).is_some())
}

/// Download reviewed release files from supported providers and publish only
/// artifacts that match the manifest hash and Forge mod metadata.
pub fn download_pinned_mods(manifest: &Manifest, cache: &Path) -> Result<usize, Error> {
    let client = Client::builder()
        .timeout(Duration::from_secs(90))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() < 3 && official_mod_file(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("unapproved mod download redirect")
            }
        }))
        .build()?;
    fs::create_dir_all(cache)?;
    let mut available = 0;
    for record in &manifest.mods {
        let Some((url, hash)) = record.file_hint.as_ref().and_then(mod_download) else {
            continue;
        };
        let destination = cache.join(format!("{hash}.jar"));
        if destination.is_file() && sha256(&destination)? == hash {
            verify_candidate(record, &destination)?;
        } else {
            const MAX_BYTES: u64 = 128 * 1024 * 1024;
            let response = client.get(url).send()?.error_for_status()?;
            let mut staged = tempfile::NamedTempFile::new_in(cache)?;
            if io::copy(&mut response.take(MAX_BYTES + 1), staged.as_file_mut())? > MAX_BYTES {
                return Err(Error::Config(format!(
                    "{} exceeded the mod download size limit",
                    record.display_name.value
                )));
            }
            if sha256(staged.path())? != hash {
                return Err(Error::Hash(destination));
            }
            verify_candidate(record, staged.path())?;
            staged.as_file().sync_all()?;
            staged.persist(&destination).map_err(|error| error.error)?;
        }
        available += 1;
    }
    Ok(available)
}

fn match_record(record: &Mod, candidate: &Candidate) -> bool {
    let range = record
        .version_range
        .as_ref()
        .and_then(|value| value.value.as_deref());
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
}

fn verify_candidate(record: &Mod, path: &Path) -> Result<(), Error> {
    let mut entries = Vec::new();
    read_metadata(path, &mut entries)?;
    if entries.iter().any(|entry| match_record(record, entry)) {
        Ok(())
    } else {
        Err(Error::Config(format!(
            "{} did not match the pinned Forge mod metadata",
            record.display_name.value
        )))
    }
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
    let mut candidates = candidates(supplied)?;
    for hash in selected
        .iter()
        .filter_map(|record| record.file_hint.as_ref()?.sha256.as_deref())
    {
        if hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            let file = cache.join(format!("{}.jar", hash.to_ascii_lowercase()));
            if file.is_file() && sha256(&file)?.eq_ignore_ascii_case(hash) {
                read_metadata(&file, &mut candidates)?;
            }
        }
    }
    let mut resolved = Vec::new();
    for record in selected {
        let expected = record
            .file_hint
            .as_ref()
            .and_then(|hint| hint.sha256.as_deref());
        let matches = candidates
            .iter()
            .filter(|candidate| match_record(record, candidate));
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
            entry_id: record.id.clone(),
            mod_id: candidate.id.clone(),
            version: candidate.version.clone(),
            sha256: hash.clone(),
            source: record.file_hint.as_ref().and_then(|hint| hint.url.clone()),
            client_only: record.client_only,
        });
    }
    resolved.sort_by(|left, right| left.mod_id.cmp(&right.mod_id));
    Ok(resolved)
}

/// Import the locally built Cloud 9 helper into the content-addressed cache.
pub fn helper_mod(path: &Path, cache: &Path) -> Result<LockedMod, Error> {
    let bytes = fs::read(path).map_err(|error| {
        Error::Config(format!(
            "Build or locate runner-helper at {} ({error})",
            path.display()
        ))
    })?;
    let mut zip = ZipArchive::new(io::Cursor::new(&bytes))?;
    let records: Value = serde_json::from_reader(zip.by_name("mcmod.info")?)?;
    let version = records
        .as_array()
        .into_iter()
        .flatten()
        .find(|record| record["modid"] == "runner-helper")
        .and_then(|record| record["version"].as_str())
        .ok_or_else(|| {
            Error::Config("the supplied helper has no runner-helper mod metadata".into())
        })?;
    let hash = format!("{:x}", Sha256::digest(&bytes));
    let target = cache.join(format!("{hash}.jar"));
    if !target.is_file() || sha256(&target)? != hash {
        atomically_write(&target, &bytes)?;
    }
    Ok(LockedMod {
        entry_id: "runner-helper".into(),
        mod_id: "runner-helper".into(),
        version: version.to_owned(),
        sha256: hash,
        source: None,
        client_only: false,
    })
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
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() || kind.is_symlink() {
            return Err(Error::Config(format!(
                "unexpected entry in instance mods: {}",
                path.display()
            )));
        }
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
#[cfg(test)]
mod download_urls {
    use super::*;

    #[test]
    fn only_exact_provider_release_files_are_downloaded() {
        let mut hint = FileHint {
            filename: Some("example mod.jar".into()),
            sha256: Some("A".repeat(64)),
            url: Some("https://www.curseforge.com/minecraft/mc-mods/example/files/3652007".into()),
            extensions: Default::default(),
        };
        let (url, hash) = mod_download(&hint).expect("pinned CurseForge release");
        assert_eq!(
            url.as_str(),
            "https://edge.forgecdn.net/files/3652/007/example%20mod.jar"
        );
        assert_eq!(hash, "a".repeat(64));
        hint.url = Some("https://cdn.modrinth.com/data/base/versions/version/file.jar".into());
        assert!(mod_download(&hint).is_some());
        hint.url =
            Some("https://cdn.modrinth.com.example.com/data/base/versions/version/file.jar".into());
        assert!(mod_download(&hint).is_none());
        hint.url = Some("https://cdn.modrinth.com/catalog.jar".into());
        assert!(mod_download(&hint).is_none());
        hint.url = Some("https://mediafilez.forgecdn.net/files/3652/657/file.jar".into());
        assert!(mod_download(&hint).is_some());
        hint.sha256 = None;
        assert!(mod_download(&hint).is_none());
    }
}
