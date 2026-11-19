mod matchup;
mod network;
mod supervise;

pub use matchup::Matchup;
pub use network::{free_port, local_ip, reachable};
pub use supervise::{Control, Status, active, request, status, supervise};

use cloud9_core::Document;
use cloud9_mc::{
    Installer, LockedMod, Profile, Registry, ServerRuntime, await_registry, capture_logs,
    discover_java, helper_mod, mod_set_hash, read_registry, resolve_mods, stage_mods, verify_mods,
};
use cloud9_world::EpisodeWorld;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use thiserror::Error;

const STATE: &str = "server.lock.json";
const WORLD: &str = "Cloud9";
pub const EULA_URL: &str = "https://www.minecraft.net/en-us/eula";

#[derive(Debug, Error)]
pub enum Error {
    #[error("{0}")]
    Config(String),
    #[error("server I/O: {0}")]
    Io(#[from] io::Error),
    #[error("server JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Manifest(#[from] cloud9_core::Error),
    #[error(transparent)]
    Minecraft(#[from] cloud9_mc::Error),
    #[error(transparent)]
    World(#[from] cloud9_world::Error),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub bind: Option<IpAddr>,
    pub port: u16,
    pub max_players: u16,
    pub motd: String,
    pub difficulty: u8,
    pub gamemode: u8,
    pub pvp: bool,
    pub view_distance: u8,
    pub ram_mib: u32,
    pub online_mode: bool,
    pub authorize_public: bool,
    pub upnp: bool,
    pub tunnel: Vec<String>,
    pub tunnel_address: Option<String>,
    pub auto_restarts: u8,
    pub ops: Vec<Player>,
    pub whitelist: Option<Vec<Player>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub uuid: String,
    pub name: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bind: None,
            port: 0,
            max_players: 8,
            motd: "Cloud 9 matchup".into(),
            difficulty: 2,
            gamemode: 0,
            pvp: true,
            view_distance: 8,
            ram_mib: 2048,
            online_mode: true,
            authorize_public: false,
            upnp: false,
            tunnel: Vec::new(),
            tunnel_address: None,
            auto_restarts: 2,
            ops: Vec::new(),
            whitelist: None,
        }
    }
}

impl Options {
    fn validate(&self) -> Result<(), Error> {
        if !(2..=1000).contains(&self.max_players)
            || !(512..=32768).contains(&self.ram_mib)
            || self.difficulty > 3
            || self.gamemode > 3
            || !(3..=32).contains(&self.view_distance)
            || self.auto_restarts > 5
            || self.motd.len() > 240
            || self.motd.contains(['\r', '\n'])
        {
            return Err(Error::Config("invalid server settings".into()));
        }
        let is_public =
            self.upnp || !self.tunnel.is_empty() || self.bind.is_some_and(|ip| !lan(ip));
        if is_public && (!self.online_mode || !self.authorize_public) {
            return Err(Error::Config(
                "Internet exposure requires online mode and explicit authorization".into(),
            ));
        }
        if !self.tunnel.is_empty() && self.tunnel_address.is_none() {
            return Err(Error::Config(
                "Set the externally reachable address for the tunnel".into(),
            ));
        }
        if let Some(address) = &self.tunnel_address {
            matchup::parse_address(address)?;
        }
        for account in self.ops.iter().chain(self.whitelist.iter().flatten()) {
            if !valid_player(account) {
                return Err(Error::Config(format!(
                    "invalid player entry {}",
                    account.name
                )));
            }
        }
        Ok(())
    }
}

fn lan(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local() || ip.is_unicast_link_local(),
    }
}

fn valid_player(player: &Player) -> bool {
    let name = &player.name;
    let uuid = &player.uuid;
    (1..=16).contains(&name.len())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && uuid.len() == 36
        && uuid.bytes().enumerate().all(|(index, byte)| {
            if [8, 13, 18, 23].contains(&index) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

pub struct Create<'a> {
    pub servers: &'a Path,
    pub slug: &'a str,
    pub cache: &'a Path,
    pub templates: &'a Path,
    pub aliases: &'a Path,
    pub local_mods: &'a Path,
    pub helper: &'a Path,
    pub java: Option<&'a Path>,
    pub profile: &'a Profile,
    pub options: Options,
    pub accept_eula: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Server {
    #[serde(skip)]
    pub root: PathBuf,
    pub profile: Profile,
    pub java: PathBuf,
    pub mods: Vec<LockedMod>,
    pub mod_set_hash: String,
    pub address: SocketAddr,
    pub options: Options,
    registry_cache: PathBuf,
    templates: PathBuf,
    aliases: PathBuf,
    runtime: ServerRuntime,
}

impl Server {
    pub fn create(document: &Document, request: Create<'_>) -> Result<Self, Error> {
        let Create {
            servers,
            slug,
            cache,
            templates,
            aliases,
            local_mods,
            helper,
            java,
            profile,
            options,
            accept_eula,
        } = request;
        profile.check_manifest(document.manifest())?;
        options.validate()?;
        if slug.is_empty()
            || slug.len() > 64
            || !slug.as_bytes()[0].is_ascii_alphanumeric()
            || !slug
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            return Err(Error::Config(
                "server folder name must contain ASCII letters, digits, underscores, or hyphens"
                    .into(),
            ));
        }
        fs::create_dir_all(servers)?;
        let servers = dunce::canonicalize(servers)?;
        let root = servers.join(slug);
        if root.exists() {
            return Err(Error::Config(format!(
                "server already exists at {}",
                root.display()
            )));
        }
        let installer = Installer::new(cache)?;
        let staging = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(&servers)?;
        let java = discover_java(profile, java)?;
        let mods_cache = installer.root.join("mods");
        let mut mods = resolve_mods(document.manifest(), local_mods, &mods_cache, true)?;
        if mods.iter().any(|entry| entry.mod_id == "runner-helper") {
            return Err(Error::Config(
                "runner-helper is supplied by the server configuration".into(),
            ));
        }
        mods.push(helper_mod(helper, &mods_cache)?);
        stage_mods(&mods, &mods_cache, &staging.path().join("mods"))?;
        fs::create_dir(staging.path().join("logs"))?;
        let runtime = ServerRuntime::install(&installer, profile, &java, staging.path())?;
        let bind = options.bind.unwrap_or_else(|| {
            let ip = local_ip();
            if lan(ip) {
                ip
            } else {
                Ipv4Addr::LOCALHOST.into()
            }
        });
        let port = if options.port == 0 {
            free_port(bind)?
        } else {
            options.port
        };
        let address = SocketAddr::new(bind, port);
        let state = Self {
            root,
            profile: profile.clone(),
            java,
            mod_set_hash: mod_set_hash(profile, &mods),
            mods,
            address,
            options,
            registry_cache: installer.root.join("registries"),
            templates: std::path::absolute(templates)?,
            aliases: std::path::absolute(aliases)?,
            runtime,
        };
        state.write_configuration(staging.path())?;
        document.save(&staging.path().join("episode.json"))?;
        json_write(&staging.path().join(STATE), &state)?;
        if accept_eula {
            write_eula(staging.path())?;
        }
        fs::rename(staging.path(), &state.root)?;
        Ok(state)
    }

    pub fn open(path: &Path) -> Result<Self, Error> {
        let root = dunce::canonicalize(path)?;
        let mut server: Self = serde_json::from_reader(File::open(root.join(STATE))?)?;
        server.root = root;
        server.options.validate()?;
        if server.address.port() == 0
            || !lan(server.address.ip()) && !server.options.authorize_public
        {
            return Err(Error::Config(
                "server bind address needs explicit public authorization".into(),
            ));
        }
        Ok(server)
    }

    /// Record EULA consent after an explicit opt-in by the operator.
    pub fn accept_eula(&self) -> Result<(), Error> {
        write_eula(&self.root)
    }

    pub fn eula_accepted(&self) -> bool {
        fs::read_to_string(self.root.join("eula.txt")).is_ok_and(|text| {
            text.lines()
                .rev()
                .find_map(|line| {
                    let (key, value) = line.split_once('=')?;
                    (key.trim() == "eula").then(|| value.trim() == "true")
                })
                .unwrap_or(false)
        })
    }

    fn write_configuration(&self, root: &Path) -> Result<(), Error> {
        let settings = &self.options;
        let operators: Vec<_> = settings
            .ops
            .iter()
            .map(|user| {
                serde_json::json!({
                    "uuid":user.uuid, "name":user.name, "level":4, "bypassesPlayerLimit":false
                })
            })
            .collect();
        json_write(&root.join("ops.json"), &operators)?;
        if let Some(players) = &settings.whitelist {
            json_write(&root.join("whitelist.json"), players)?;
        }
        self.write_properties(root, self.address, WORLD, self.options.online_mode)
    }

    fn write_properties(
        &self,
        root: &Path,
        address: SocketAddr,
        level: &str,
        online: bool,
    ) -> Result<(), Error> {
        let settings = &self.options;
        let properties = format!(
            "level-name={level}\ngamemode={}\ndifficulty={}\npvp={}\nspawn-protection=0\nmax-players={}\nmotd={}\nview-distance={}\nallow-flight=true\nonline-mode={online}\nserver-ip={}\nserver-port={}\nwhite-list={}\nenable-rcon=false\nenable-query=false\n",
            settings.gamemode,
            settings.difficulty,
            settings.pvp,
            settings.max_players,
            property_text(&settings.motd),
            settings.view_distance,
            address.ip(),
            address.port(),
            settings.whitelist.is_some(),
        );
        publish(&root.join("server.properties"), properties.as_bytes())
    }

    fn preflight(&self) -> Result<(), Error> {
        if !self.eula_accepted() {
            return Err(Error::Config(format!(
                "Read {EULA_URL} and explicitly accept the EULA to start this server"
            )));
        }
        discover_java(&self.profile, Some(&self.java))?;
        verify_mods(&self.mods, &self.root.join("mods"))?;
        if !self
            .mods
            .iter()
            .any(|entry| entry.mod_id == "runner-helper")
            || self.mod_set_hash != mod_set_hash(&self.profile, &self.mods)
        {
            return Err(Error::Config("server mod set differs from its lock".into()));
        }
        self.runtime.verify(&self.root)?;
        Ok(())
    }

    fn command(&self) -> Command {
        self.runtime.command(
            &self.root,
            &self.java,
            &self.mod_set_hash,
            self.options.ram_mib,
        )
    }

    fn registry(&self) -> Result<Registry, Error> {
        let cache = self
            .registry_cache
            .join(format!("{}.json", self.mod_set_hash));
        if cache.is_file() {
            return Ok(read_registry(&cache, &self.mod_set_hash)?);
        }
        let dump = self.root.join("registry_dump.json");
        if dump.is_file() {
            let registry = read_registry(&dump, &self.mod_set_hash)?;
            json_write(&cache, &registry)?;
            return Ok(registry);
        }
        let loopback = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let address = SocketAddr::new(loopback, free_port(loopback)?);
        self.write_properties(&self.root, address, "registry-probe", true)?;
        let result = (|| -> Result<Registry, Error> {
            let mut child = self
                .command()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?;
            let (log, receiver) = mpsc::channel();
            if let Some(output) = child.stdout.take() {
                capture_logs(output, log.clone(), Vec::new());
            }
            if let Some(output) = child.stderr.take() {
                capture_logs(output, log, Vec::new());
            }
            let registry = await_registry(&mut child, &receiver, &dump, &self.mod_set_hash);
            supervise::stop_child(&mut child, Duration::from_secs(20))?;
            Ok(registry?)
        })();
        self.write_properties(&self.root, self.address, WORLD, self.options.online_mode)?;
        let registry = result?;
        json_write(&cache, &registry)?;
        Ok(registry)
    }

    fn round(&self) -> Result<(Document, EpisodeWorld), Error> {
        let document = Document::load(&self.root.join("episode.json"))?;
        self.profile.check_manifest(document.manifest())?;
        let registry = self.registry()?;
        let round = EpisodeWorld::open(
            &document,
            self.root.join(WORLD),
            self.root.join("round-snapshot"),
            &self.templates,
            &self.aliases,
            &self.mods,
            &registry,
            &self.mod_set_hash,
            "cloud9-round",
        )?;
        Ok((document, round))
    }

    fn lock(&self) -> Result<File, Error> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join("supervisor.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
            if error.kind() == io::ErrorKind::WouldBlock {
                Error::Config("server already has an active supervisor".into())
            } else {
                Error::Io(error)
            }
        })?;
        Ok(lock)
    }
}

fn write_eula(directory: &Path) -> Result<(), Error> {
    publish(
        &directory.join("eula.txt"),
        format!("# Explicitly accepted by the Cloud 9 operator. {EULA_URL}\neula=true\n")
            .as_bytes(),
    )
}

fn property_text(text: &str) -> String {
    let mut output = String::new();
    use std::fmt::Write as _;
    for unit in text.encode_utf16() {
        match unit {
            0x5c => output.push_str("\\\\"),
            0x20..=0x7e => output.push(char::from(unit as u8)),
            _ => {
                let _ = write!(output, "\\u{unit:04x}");
            }
        }
    }
    output
}

fn json_write(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    publish(path, &serde_json::to_vec_pretty(value)?)
}

fn publish(path: &Path, data: &[u8]) -> Result<(), Error> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut staging = tempfile::NamedTempFile::new_in(parent)?;
    staging.write_all(data)?;
    staging.as_file().sync_all()?;
    staging.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_keep_authentication_and_guard_exposure() {
        let mut options = Options::default();
        assert!(options.online_mode);
        options.upnp = true;
        assert!(options.validate().is_err());
        options.authorize_public = true;
        assert!(options.validate().is_ok());
        options.online_mode = false;
        assert!(options.validate().is_err());
        assert_eq!(property_text("hello ☁"), "hello \\u2601");
    }
}
