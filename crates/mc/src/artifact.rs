use crate::{Error, Profile, atomically_write, safe_component};
use fs2::FileExt;
use reqwest::{StatusCode, Url, blocking::Client, header};
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::Duration,
};
use zip::ZipArchive;

const INDEX: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const FORGE: &str = "https://maven.minecraftforge.net/";
const LIBRARIES: &str = "https://libraries.minecraft.net/";
const ASSETS: &str = "https://resources.download.minecraft.net/";

pub struct Installer {
    pub(crate) http: Client,
    pub root: PathBuf,
}

fn official(url: &Url) -> bool {
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && matches!(
            url.host_str(),
            Some(
                "piston-meta.mojang.com"
                    | "piston-data.mojang.com"
                    | "launchermeta.mojang.com"
                    | "launcher.mojang.com"
                    | "libraries.minecraft.net"
                    | "resources.download.minecraft.net"
                    | "maven.minecraftforge.net"
            )
        )
}

impl Installer {
    pub fn new(path: &Path) -> Result<Self, Error> {
        fs::create_dir_all(path)?;
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(240))
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() < 3 && official(attempt.url()) {
                    attempt.follow()
                } else {
                    attempt.error("unapproved artifact redirect")
                }
            }))
            .build()?;
        Ok(Self {
            http,
            root: dunce::canonicalize(path)?,
        })
    }

    fn url(address: &str) -> Result<Url, Error> {
        let url = Url::parse(address).map_err(|_| Error::Config("invalid artifact URL".into()))?;
        if !official(&url) {
            return Err(Error::Config(format!(
                "unsupported artifact origin {address}"
            )));
        }
        Ok(url)
    }

    fn text(&self, address: &str) -> Result<String, Error> {
        Ok(self
            .http
            .get(Self::url(address)?)
            .send()?
            .error_for_status()?
            .text()?)
    }

    /// Resume a download under an exclusive lock and publish only a verified file.
    pub fn get(&self, address: &str, sha1s: &[&str], destination: &Path) -> Result<(), Error> {
        let url = Self::url(address)?;
        if sha1s.is_empty()
            || sha1s
                .iter()
                .any(|hash| hash.len() != 40 || !hash.bytes().all(|c| c.is_ascii_hexdigit()))
        {
            return Err(Error::Config(format!("malformed SHA-1 for {address}")));
        }
        let parent = destination
            .parent()
            .ok_or_else(|| Error::Config("artifact path missing parent".into()))?;
        fs::create_dir_all(parent)?;
        if verify_sha1(destination, sha1s)? {
            return Ok(());
        }
        let name = destination
            .file_name()
            .ok_or_else(|| Error::Config("artifact path missing filename".into()))?;
        let mut partial_name = name.to_os_string();
        partial_name.push(".part");
        let partial = parent.join(&partial_name);
        partial_name.push(".lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(parent.join(partial_name))?;
        FileExt::lock_exclusive(&lock)?;
        if verify_sha1(destination, sha1s)? {
            return Ok(());
        }
        if !verify_sha1(&partial, sha1s)? {
            let offset = fs::metadata(&partial).map_or(0, |metadata| metadata.len());
            let mut request = self.http.get(url.clone());
            if offset > 0 {
                request = request.header(header::RANGE, format!("bytes={offset}-"));
            }
            let mut response = request.send()?;
            if response.status() == StatusCode::RANGE_NOT_SATISFIABLE && offset > 0 {
                response = self.http.get(url).send()?;
            }
            let mut response = response.error_for_status()?;
            let append = response.status() == StatusCode::PARTIAL_CONTENT;
            if append {
                let range = response
                    .headers()
                    .get(header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok());
                if !range.is_some_and(|value| value.starts_with(&format!("bytes {offset}-"))) {
                    fs::remove_file(&partial)?;
                    return Err(Error::Config("unexpected download content range".into()));
                }
            }
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .append(append)
                .truncate(!append)
                .open(&partial)?;
            io::copy(&mut response, &mut file)?;
            file.sync_all()?;
        }
        if !verify_sha1(&partial, sha1s)? {
            fs::remove_file(&partial)?;
            return Err(Error::Hash(destination.to_owned()));
        }
        let mut verified = tempfile::NamedTempFile::new_in(parent)?;
        io::copy(&mut File::open(&partial)?, &mut verified)?;
        verified.as_file().sync_all()?;
        verified.persist(destination).map_err(|error| error.error)?;
        fs::remove_file(partial)?;
        tracing::debug!(%address, path = %destination.display(), "download verified");
        Ok(())
    }

    pub(crate) fn download_entry(&self, entry: &Value, path: &Path) -> Result<(), Error> {
        self.get(string(entry, "url")?, &[string(entry, "sha1")?], path)
    }

    pub(crate) fn vanilla(&self, profile: &Profile) -> Result<Value, Error> {
        if !safe_component(&profile.mc_version) {
            return Err(Error::Config("invalid Minecraft version ID".into()));
        }
        let index_path = self.root.join("version_manifest_v2.json");
        let stale = fs::metadata(&index_path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|time| time.elapsed().ok())
            .is_none_or(|age| age > Duration::from_secs(86400));
        if stale {
            let payload = self.text(INDEX)?;
            let _: Value = serde_json::from_str(&payload)?;
            atomically_write(&index_path, payload.as_bytes())?;
        }
        let index: Value = serde_json::from_reader(File::open(index_path)?)?;
        let entry = array(&index, "versions")?
            .iter()
            .find(|entry| entry["id"] == profile.mc_version)
            .ok_or_else(|| {
                Error::Config(format!("Minecraft {} unavailable", profile.mc_version))
            })?;
        let path = self
            .root
            .join("versions")
            .join(&profile.mc_version)
            .join("version.json");
        self.download_entry(entry, &path)?;
        Ok(serde_json::from_reader(File::open(path)?)?)
    }

    pub fn forge_installer(&self, profile: &Profile) -> Result<PathBuf, Error> {
        self.forge_artifact(profile, "installer", &profile.installer_sha1)
    }

    fn forge_artifact(&self, profile: &Profile, kind: &str, hash: &str) -> Result<PathBuf, Error> {
        let coordinate = &profile.forge_coordinate;
        if !safe_component(coordinate) {
            return Err(Error::Config("invalid Forge coordinate".into()));
        }
        let filename = format!("forge-{coordinate}-{kind}.jar");
        let path = self.root.join("forge").join(&filename);
        self.get(
            &format!("{FORGE}net/minecraftforge/forge/{coordinate}/{filename}"),
            &[hash],
            &path,
        )?;
        Ok(path)
    }

    pub(crate) fn legacy_forge(&self, profile: &Profile) -> Result<(Value, PathBuf), Error> {
        if profile.loader != "forge" || !safe_component(&profile.forge_version_id) {
            return Err(Error::Config("unsupported version profile".into()));
        }
        let archive = self.forge_installer(profile)?;
        let mut zip = ZipArchive::new(File::open(archive)?)?;
        let config: Value = serde_json::from_reader(zip.by_name("install_profile.json")?)?;
        let forge = config["versionInfo"].clone();
        if forge["id"] != profile.forge_version_id || forge["inheritsFrom"] != profile.mc_version {
            return Err(Error::Config(
                "Forge installer metadata does not match the selected profile".into(),
            ));
        }
        let path = self.root.join("versions").join(&profile.forge_version_id);
        let json = path.join(format!("{}.json", profile.forge_version_id));
        let jar = path.join(format!("{}.jar", profile.forge_version_id));
        if !json.is_file() || !verify_sha1(&jar, &[&profile.universal_sha1])? {
            let source = self.forge_artifact(profile, "universal", &profile.universal_sha1)?;
            atomically_write(&jar, &fs::read(source)?)?;
            atomically_write(&json, &serde_json::to_vec_pretty(&forge)?)?;
        } else if serde_json::from_reader::<_, Value>(File::open(&json)?)
            .ok()
            .as_ref()
            != Some(&forge)
        {
            atomically_write(&json, &serde_json::to_vec_pretty(&forge)?)?;
        }
        Ok((forge, jar))
    }

    pub(crate) fn libraries(
        &self,
        metadata: &[&Value],
        forge_jar: &Path,
        native_dir: &Path,
    ) -> Result<Vec<PathBuf>, Error> {
        let mut paths = Vec::new();
        let mut groups = std::collections::HashSet::new();
        let platform = platform()?;
        for index in metadata {
            for library in array(index, "libraries")? {
                if !allowed(library, platform) {
                    continue;
                }
                let name = string(library, "name")?;
                let parts: Vec<_> = name.split(':').collect();
                let [group, artifact, version] = parts.as_slice() else {
                    return Err(Error::Config(format!("unsupported library name {name}")));
                };
                let classifier = library["natives"][platform].as_str();
                if !groups.insert((*group, *artifact, classifier)) {
                    continue;
                }
                if name.starts_with("net.minecraftforge:forge:") {
                    paths.push(forge_jar.to_owned());
                    continue;
                }
                if let Some(artifact) = library["downloads"].get("artifact") {
                    let path = self
                        .root
                        .join("libraries")
                        .join(relative(string(artifact, "path")?)?);
                    self.download_entry(artifact, &path)?;
                    paths.push(path);
                } else if classifier.is_none() {
                    if ![*group, *artifact, *version]
                        .iter()
                        .all(|item| safe_component(item))
                    {
                        return Err(Error::Config(format!("invalid library coordinate {name}")));
                    }
                    let coordinate = format!(
                        "{}/{artifact}/{version}/{artifact}-{version}.jar",
                        group.replace('.', "/")
                    );
                    let path = self.root.join("libraries").join(relative(&coordinate)?);
                    let base = library["url"].as_str().unwrap_or(LIBRARIES);
                    let url = format!("{}/{coordinate}", base.trim_end_matches('/'));
                    let provided = library["checksums"]
                        .as_array()
                        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                        .unwrap_or_default();
                    if provided.is_empty() {
                        let checksum = self.text(&format!("{url}.sha1"))?;
                        let hash = checksum.split_whitespace().next().unwrap_or("");
                        self.get(&url, &[hash], &path)?;
                    } else {
                        self.get(&url, &provided, &path)?;
                    }
                    paths.push(path);
                }
                if let Some(classifier) = classifier {
                    let classifier = classifier.replace("${arch}", &usize::BITS.to_string());
                    let native = &library["downloads"]["classifiers"][&classifier];
                    let path = self
                        .root
                        .join("libraries")
                        .join(relative(string(native, "path")?)?);
                    self.download_entry(native, &path)?;
                    let excludes = library["extract"]["exclude"]
                        .as_array()
                        .map(|values| values.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                        .unwrap_or_default();
                    extract_natives(&path, native_dir, &excludes)?;
                }
            }
        }
        Ok(paths)
    }

    pub(crate) fn assets(&self, vanilla: &Value) -> Result<String, Error> {
        let index = &vanilla["assetIndex"];
        let id = string(index, "id")?;
        if !safe_component(id) {
            return Err(Error::Config("invalid asset index ID".into()));
        }
        let root = self.root.join("assets");
        let path = root.join("indexes").join(format!("{id}.json"));
        self.download_entry(index, &path)?;
        let payload: Value = serde_json::from_reader(File::open(path)?)?;
        let entries = payload["objects"]
            .as_object()
            .ok_or_else(|| Error::Config("asset index lacks objects".into()))?;
        // A bounded worker pool shares the HTTP connection pool and stops after the first failing batch.
        let entries: Vec<_> = entries.iter().collect();
        for batch in entries.chunks(24) {
            std::thread::scope(|scope| {
                let workers: Vec<_> = batch
                    .chunks(6)
                    .map(|chunk| {
                        let root = &root;
                        let payload = &payload;
                        scope.spawn(move || -> Result<(), Error> {
                            for (name, entry) in chunk {
                                let hash = string(entry, "hash")?;
                                if hash.len() != 40
                                    || !hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                                {
                                    return Err(Error::Config("invalid asset SHA-1".into()));
                                }
                                let key = format!("{}/{hash}", &hash[..2]);
                                let path = root.join("objects").join(&key);
                                self.get(&format!("{ASSETS}{key}"), &[hash], &path)?;
                                if payload["virtual"] == true {
                                    let dest = root.join("virtual").join(id).join(relative(name)?);
                                    if !verify_sha1(&dest, &[hash])? {
                                        atomically_write(&dest, &fs::read(path)?)?;
                                    }
                                }
                            }
                            Ok(())
                        })
                    })
                    .collect();
                for worker in workers {
                    worker
                        .join()
                        .map_err(|_| Error::Config("asset worker failed".into()))??;
                }
                Ok::<_, Error>(())
            })?;
        }
        Ok(id.to_owned())
    }
}

pub(crate) fn verify_sha1(path: &Path, hashes: &[&str]) -> Result<bool, Error> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let mut sha = Sha1::new();
    io::copy(&mut file, &mut sha)?;
    let digest = format!("{:x}", sha.finalize());
    Ok(hashes.iter().any(|hash| digest.eq_ignore_ascii_case(hash)))
}

pub(crate) fn string<'a>(value: &'a Value, field: &str) -> Result<&'a str, Error> {
    value[field]
        .as_str()
        .ok_or_else(|| Error::Config(format!("version metadata lacks {field}")))
}

fn array<'a>(value: &'a Value, field: &str) -> Result<&'a [Value], Error> {
    value[field]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| Error::Config(format!("version metadata lacks {field}")))
}

fn relative(value: &str) -> Result<&Path, Error> {
    let path = Path::new(value);
    if !value.is_empty()
        && !value.contains(['\\', ':'])
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(path)
    } else {
        Err(Error::Config(format!("invalid relative path {value}")))
    }
}

pub(crate) fn platform() -> Result<&'static str, Error> {
    match std::env::consts::OS {
        "windows" => Ok("windows"),
        "linux" => Ok("linux"),
        "macos" => Ok("osx"),
        other => Err(Error::Config(format!("unsupported host platform {other}"))),
    }
}

fn allowed(library: &Value, platform: &str) -> bool {
    let Some(rules) = library["rules"].as_array() else {
        return true;
    };
    let mut permit = false;
    for rule in rules {
        let os_matches = rule["os"]["name"]
            .as_str()
            .is_none_or(|name| name == platform);
        let arch_matches = rule["os"]["arch"]
            .as_str()
            .is_none_or(|arch| arch == std::env::consts::ARCH);
        if os_matches
            && arch_matches
            && rule["features"].is_null()
            && rule["os"]["version"].is_null()
        {
            permit = rule["action"] == "allow";
        }
    }
    permit
}

fn extract_natives(archive: &Path, target: &Path, exclude: &[&str]) -> Result<(), Error> {
    let mut zip = ZipArchive::new(File::open(archive)?)?;
    for index in 0..zip.len() {
        let mut file = zip.by_index(index)?;
        if file.is_dir() || exclude.iter().any(|prefix| file.name().starts_with(prefix)) {
            continue;
        }
        let path = target.join(relative(file.name())?);
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;
        atomically_write(&path, &data)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_paths_and_windows_rules() {
        assert!(relative("org/example/lib.jar").is_ok());
        assert!(relative("../escape").is_err());
        assert!(relative("a\\b.jar").is_err());
        let rules = serde_json::json!({"rules":[{"action":"allow"},{"action":"disallow","os":{"name":"osx"}}]});
        assert!(allowed(&rules, "windows"));
        assert!(!allowed(&rules, "osx"));
        assert!(!official(
            &Url::parse("https://maven.minecraftforge.net.example.net/").expect("url")
        ));
    }
}
