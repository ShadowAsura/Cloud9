use crate::{
    Account, Error, Installer, LockedMod, LogEvent, Profile,
    artifact::{platform, string},
    atomically_write, capture_logs, discover_java, resolve_mods, safe_component, stage_mods,
    verify_mods,
};
use cloud9_core::{Document, Manifest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::mpsc::Sender,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct Instance {
    #[serde(skip)]
    pub root: PathBuf,
    pub profile: Profile,
    pub java: PathBuf,
    pub mods: Vec<LockedMod>,
    pub mod_set_hash: String,
}

impl Instance {
    /// Read instance metadata for display. Launches must use open or verify.
    pub fn inspect(directory: &Path) -> Result<Self, Error> {
        let root = dunce::canonicalize(directory)?;
        let mut state: Self = serde_json::from_slice(&fs::read(root.join("runner.lock.json"))?)?;
        state.root = root;
        Ok(state)
    }

    pub fn open(directory: &Path) -> Result<Self, Error> {
        let state = Self::inspect(directory)?;
        state.verify()?;
        Ok(state)
    }

    /// Hold the file for the entire session to prevent concurrent clients
    /// from changing the same instance saves.
    pub fn session_lock(&self) -> Result<fs::File, Error> {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("cloud9-session.lock"))?;
        fs2::FileExt::try_lock_exclusive(&file)
            .map_err(|error| Error::Config(format!("Instance is already in use ({error})")))?;
        Ok(file)
    }

    pub fn verify(&self) -> Result<(), Error> {
        discover_java(&self.profile, Some(&self.java))?;
        verify_mods(&self.mods, &self.root.join("mods"))?;
        if self.mod_set_hash != mod_set_hash(&self.profile, &self.mods) {
            return Err(Error::Config(
                "mod-set hash differs from instance lock".into(),
            ));
        }
        Ok(())
    }
}

pub fn mod_set_hash(profile: &Profile, mods: &[LockedMod]) -> String {
    let mut combined = Sha256::new();
    for value in [
        &profile.mc_version,
        &profile.loader,
        &profile.loader_version,
    ] {
        combined.update(value);
        combined.update([0]);
    }
    let mut hashes: Vec<_> = mods.iter().map(|entry| &entry.sha256).collect();
    hashes.sort_unstable();
    hashes.dedup();
    for hash in hashes {
        combined.update(hash);
        combined.update([0]);
    }
    format!("{:x}", combined.finalize())
}

pub fn create_instance(
    instances: &Path,
    cache: &Path,
    document: &Document,
    profile: &Profile,
    supplied: &Path,
    java: Option<&Path>,
) -> Result<Instance, Error> {
    create_instance_inner(instances, cache, document, profile, supplied, java, None)
}

pub fn create_instance_with_helper(
    instances: &Path,
    cache: &Path,
    document: &Document,
    profile: &Profile,
    supplied: &Path,
    java: Option<&Path>,
    helper: &Path,
) -> Result<Instance, Error> {
    create_instance_inner(
        instances,
        cache,
        document,
        profile,
        supplied,
        java,
        Some(helper),
    )
}

/// Prepare in a private sibling directory and publish only a complete, isolated instance.
fn create_instance_inner(
    instances: &Path,
    cache: &Path,
    document: &Document,
    profile: &Profile,
    supplied: &Path,
    java: Option<&Path>,
    helper: Option<&Path>,
) -> Result<Instance, Error> {
    profile.check_manifest(document.manifest())?;
    let episode = &document.manifest().episode.video_id;
    if !safe_component(episode) {
        return Err(Error::Config("invalid episode ID for local path".into()));
    }
    let java = discover_java(profile, java)?;
    let mut mods = resolve_mods(document.manifest(), supplied, &cache.join("mods"), false)?;
    if let Some(helper) = helper {
        if mods.iter().any(|entry| entry.mod_id == "runner-helper") {
            return Err(Error::Config(
                "runner-helper is supplied by the run configuration".into(),
            ));
        }
        mods.push(crate::helper_mod(helper, &cache.join("mods"))?);
    }
    fs::create_dir_all(instances)?;
    let root = dunce::canonicalize(instances)?;
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(&root)?;
    for name in ["saves", "logs", "config", "natives"] {
        fs::create_dir(staging.path().join(name))?;
    }
    stage_mods(&mods, &cache.join("mods"), &staging.path().join("mods"))?;
    document
        .save(&staging.path().join("episode.json"))
        .map_err(|error| Error::Config(error.to_string()))?;
    let stamp = staging
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Error::Config("temporary instance name is not UTF-8".into()))?;
    let final_path = root.join(format!(
        "{episode}-{}",
        stamp.trim_start_matches(".staging-")
    ));
    let instance = Instance {
        root: final_path,
        profile: profile.clone(),
        java,
        mod_set_hash: mod_set_hash(profile, &mods),
        mods,
    };
    atomically_write(
        &staging.path().join("runner.lock.json"),
        &serde_json::to_vec_pretty(&instance)?,
    )?;
    if instance.root.exists() {
        return Err(Error::Config("instance already exists".into()));
    }
    fs::rename(staging.path(), &instance.root)?;
    Ok(instance)
}

pub struct ClientRuntime {
    classpath: Vec<PathBuf>,
    assets: PathBuf,
    asset_index: String,
    metadata: Value,
    logging_arg: Option<String>,
}

impl ClientRuntime {
    pub fn prepare(installer: &Installer, instance: &Instance) -> Result<Self, Error> {
        instance.verify()?;
        let profile = &instance.profile;
        let vanilla = installer.vanilla(profile)?;
        let (forge, forge_jar) = installer.legacy_forge(profile)?;
        let jar = installer
            .root
            .join("versions")
            .join(&profile.mc_version)
            .join("client.jar");
        installer.download_entry(&vanilla["downloads"]["client"], &jar)?;
        let mut classpath = installer.libraries(
            &[&forge, &vanilla],
            &forge_jar,
            &instance.root.join("natives"),
        )?;
        classpath.push(jar);
        let asset_index = installer.assets(&vanilla)?;
        let logging = &vanilla["logging"]["client"];
        let logging_arg = if logging.is_object() {
            let file = &logging["file"];
            let id = string(file, "id")?;
            if !safe_component(id) {
                return Err(Error::Config("invalid logging configuration ID".into()));
            }
            let path = installer.root.join("log_configs").join(id);
            installer.download_entry(file, &path)?;
            Some(string(logging, "argument")?.replace("${path}", &path.to_string_lossy()))
        } else {
            None
        };
        Ok(Self {
            classpath,
            assets: installer.root.join("assets"),
            asset_index,
            metadata: forge,
            logging_arg,
        })
    }

    pub fn launch(
        &self,
        instance: &Instance,
        account: &Account,
        ram_mib: u32,
        to_server: Option<(&str, u16)>,
        log: Sender<LogEvent>,
    ) -> Result<Child, Error> {
        instance.verify()?;
        if !(512..=32768).contains(&ram_mib) {
            return Err(Error::Config(
                "RAM must be between 512 and 32768 MiB".into(),
            ));
        }
        if to_server.is_some() && !account.authenticated() {
            return Err(Error::Config(
                "Microsoft sign-in required for joining a server".into(),
            ));
        }
        let classpath = std::env::join_paths(&self.classpath)
            .map_err(|error| Error::Config(error.to_string()))?;
        let game_dir = instance.root.to_string_lossy();
        let assets = self.assets.to_string_lossy();
        let vars = [
            ("${auth_player_name}", account.name.as_str()),
            ("${auth_uuid}", account.uuid.as_str()),
            ("${auth_access_token}", account.token.as_str()),
            (
                "${version_name}",
                instance.profile.forge_version_id.as_str(),
            ),
            ("${game_directory}", game_dir.as_ref()),
            ("${assets_root}", assets.as_ref()),
            ("${asset_index_name}", self.asset_index.as_str()),
            ("${assets_index_name}", self.asset_index.as_str()),
            ("${user_properties}", "{}"),
            ("${user_type}", account.kind),
        ];
        let args = string(&self.metadata, "minecraftArguments")?
            .split_whitespace()
            .map(|word| {
                if let Some((_, value)) = vars.iter().find(|(key, _)| *key == word) {
                    Ok(*value)
                } else if word.contains("${") {
                    Err(Error::Config(format!(
                        "unsupported argument placeholder {word}"
                    )))
                } else {
                    Ok(word)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut command = Command::new(&instance.java);
        command
            .current_dir(&instance.root)
            .arg(format!("-Xmx{ram_mib}M"))
            .arg(format!(
                "-Djava.library.path={}",
                instance.root.join("natives").display()
            ))
            .arg(format!("-Dcloud9.modSetHash={}", instance.mod_set_hash))
            .arg("-Dlog4j2.formatMsgNoLookups=true");
        if platform()? == "osx" {
            command.arg("-XstartOnFirstThread");
        }
        if let Some(arg) = &self.logging_arg {
            command.arg(arg);
        }
        command
            .arg("-cp")
            .arg(classpath)
            .arg(string(&self.metadata, "mainClass")?)
            .args(args);
        if let Some((host, port)) = to_server {
            command
                .arg("--server")
                .arg(host)
                .arg("--port")
                .arg(port.to_string());
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let secrets = if account.authenticated() {
            vec![account.token.clone()]
        } else {
            Vec::new()
        };
        if let Some(stdout) = child.stdout.take() {
            capture_logs(stdout, log.clone(), secrets.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            capture_logs(stderr, log, secrets);
        }
        tracing::info!(pid = child.id(), "Minecraft launched");
        Ok(child)
    }
}

/// Count item references so a run plan can require a matching registry dump.
pub fn item_count(manifest: &Manifest) -> usize {
    manifest
        .villagers
        .iter()
        .flat_map(|villager| &villager.trades)
        .map(|trade| trade.buy.len() + usize::from(trade.buy_b.is_some()) + 1)
        .sum::<usize>()
        + manifest.center.blocks.len()
        + manifest.center.generators.len()
        + manifest
            .center
            .chests
            .iter()
            .map(|chest| chest.slots.len())
            .sum::<usize>()
}
