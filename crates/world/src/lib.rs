mod region;

use cloud9_core::{Document, Item, Mode};
use cloud9_mc::{Alias, Registry, Resolution};
use fastnbt::Value as Tag;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap},
    fs::{self, File},
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("world I/O: {0}")]
    Io(#[from] io::Error),
    #[error("world JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("world NBT: {0}")]
    Nbt(#[from] fastnbt::error::Error),
    #[error("{0}")]
    Setup(String),
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub spawn: [i32; 3],
    pub gamerules: BTreeMap<String, String>,
    pub teams: Vec<Value>,
    pub kits: Vec<Value>,
    pub tile_entities: Vec<TileEntity>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            spawn: [0, 65, 0],
            gamerules: BTreeMap::from([
                ("keepInventory".into(), "true".into()),
                ("doMobSpawning".into(), "false".into()),
            ]),
            teams: Vec::new(),
            kits: Vec::new(),
            tile_entities: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TileEntity {
    pub pos: [i32; 3],
    pub id: String,
    pub fields: HashMap<String, Tag>,
}

pub trait SetupApplier {
    /// Apply a validated setup to a stopped world.
    fn apply(&self, world: &Path, setup: &Setup) -> Result<(), Error>;
}

pub struct Offline;
pub struct Helper;
pub struct Setup {
    pub settings: Settings,
    helper_data: Value,
}

impl Setup {
    /// Validate registry IDs before writing concrete item names for the helper.
    pub fn verified(
        document: &Document,
        run_id: &str,
        hash: &str,
        registry: &Registry,
        aliases: &BTreeMap<String, Alias>,
        starter: bool,
    ) -> Result<Self, Error> {
        registry
            .check_hash(hash)
            .map_err(|error| Error::Setup(error.to_string()))?;
        let manifest = document.manifest();
        let mode = manifest.mode.value;
        let mut settings: Settings =
            serde_json::from_value(document.source()["map"]["overrides"].clone())?;
        if starter && mode == Mode::LuckyBlockBedwars && settings.teams.is_empty() {
            settings.teams = vec![
                json!({"id":"cloud9-red","name":"Red","color":"red","spawn":[-22,65,0]}),
                json!({"id":"cloud9-blue","name":"Blue","color":"blue","spawn":[22,65,0]}),
            ];
        }
        let mut villagers = document.source()["villagers"].clone();
        let mut center = document.source()["center"].clone();
        let mut kits = Value::Array(settings.kits.clone());
        resolve_items(&mut villagers, registry, aliases, false)?;
        resolve_items(&mut center["blocks"], registry, aliases, true)?;
        resolve_items(&mut center["chests"], registry, aliases, false)?;
        resolve_items(&mut center["generators"], registry, aliases, false)?;
        resolve_items(&mut kits, registry, aliases, false)?;
        let helper_data = json!({
            "schema_version": 1, "run_id": run_id, "mode": mode,
            "spawn": settings.spawn, "gamerules": settings.gamerules,
            "teams": settings.teams, "kits": kits, "starter_layout": starter,
            "villagers": villagers, "center": center,
        });
        Ok(Self {
            settings,
            helper_data,
        })
    }
}

fn resolve_items(
    node: &mut Value,
    registry: &Registry,
    aliases: &BTreeMap<String, Alias>,
    block: bool,
) -> Result<(), Error> {
    if let Some(entries) = node.as_array_mut() {
        for entry in entries {
            resolve_items(entry, registry, aliases, block)?;
        }
    } else if let Some(fields) = node.as_object_mut() {
        if let Some(source) = fields.get_mut("item") {
            let item: Item = serde_json::from_value(source.clone())?;
            let id = match registry.resolve(&item, aliases, block) {
                Resolution::Exact(id) => id,
                Resolution::Suggestion(id) => {
                    return Err(Error::Setup(format!(
                        "Edit alias: {:?} may refer to {id}",
                        item.raw_text
                    )));
                }
                Resolution::Unresolved => {
                    return Err(Error::Setup(format!(
                        "Edit alias: unknown item {:?}",
                        item.raw_text
                    )));
                }
            };
            if let Some(alias) = aliases
                .get(&item.raw_text.to_lowercase())
                .or_else(|| item.id.as_ref().and_then(|id| aliases.get(id)))
            {
                if let Some(meta) = alias.meta {
                    source["meta"] = json!(meta);
                }
                if let Some(nbt) = &alias.nbt {
                    source["nbt"] = json!(nbt);
                }
            }
            source["id"] = json!(id);
        }
        for (key, value) in fields {
            if !matches!(key.as_str(), "item" | "nbt") {
                resolve_items(value, registry, aliases, block)?;
            }
        }
    }
    Ok(())
}

pub fn load_aliases(
    directory: &Path,
    mods: &[cloud9_mc::LockedMod],
) -> Result<BTreeMap<String, Alias>, Error> {
    let mut result = BTreeMap::new();
    for record in mods {
        if [&record.mod_id, &record.version].iter().any(|segment| {
            segment.is_empty()
                || !segment.as_bytes()[0].is_ascii_alphanumeric()
                || !segment
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b".-_".contains(&c))
        }) {
            return Err(Error::Setup("invalid alias path segment".into()));
        }
        let path = directory
            .join(&record.mod_id)
            .join(format!("{}.json", record.version));
        if path.is_file() {
            let aliases: BTreeMap<String, Alias> = serde_json::from_reader(File::open(&path)?)?;
            for (name, alias) in aliases {
                let name = name.to_lowercase();
                if result.insert(name.clone(), alias).is_some() {
                    return Err(Error::Setup(format!("ambiguous item alias {name}")));
                }
            }
        }
    }
    Ok(result)
}

impl SetupApplier for Offline {
    fn apply(&self, world: &Path, setup: &Setup) -> Result<(), Error> {
        let path = world.join("level.dat");
        let mut state = read_level(&path)?;
        let data = compound(&mut state, "Data")?;
        for (axis, coordinate) in ["SpawnX", "SpawnY", "SpawnZ"]
            .into_iter()
            .zip(setup.settings.spawn)
        {
            data.insert(axis.into(), Tag::Int(coordinate));
        }
        let rules = compound(data, "GameRules")?;
        for (name, value) in &setup.settings.gamerules {
            rules.insert(name.clone(), Tag::String(value.clone()));
        }
        for tile in &setup.settings.tile_entities {
            region::update_tile(world, tile)?;
        }
        write_level(&path, &state)
    }
}

impl SetupApplier for Helper {
    fn apply(&self, world: &Path, setup: &Setup) -> Result<(), Error> {
        let path = world.join("runner_setup.json");
        publish(&path, &serde_json::to_vec_pretty(&setup.helper_data)?)?;
        tracing::debug!(path = %path.display(), "helper setup written");
        Ok(())
    }
}

pub struct EpisodeWorld {
    pub world: PathBuf,
    snapshot: PathBuf,
    setup: Setup,
}

impl EpisodeWorld {
    pub fn open(
        document: &Document,
        world: PathBuf,
        snapshot: PathBuf,
        templates: &Path,
        alias_root: &Path,
        mods: &[cloud9_mc::LockedMod],
        registry: &Registry,
        hash: &str,
        run_id: &str,
    ) -> Result<Self, Error> {
        let manifest = document.manifest();
        let name = &manifest.map.template_id;
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(Error::Setup("unsupported world template ID".into()));
        }
        let template = templates.join(name);
        let starter = match manifest.mode.value {
            Mode::ChallengeGames => name == "challenge_games",
            Mode::LuckyBlockBedwars => name == "lucky_block_bedwars",
        };
        if starter && !template.exists() {
            create_builtin_template(templates, manifest.mode.value)?;
        }
        let aliases = load_aliases(alias_root, mods)?;
        let setup = Setup::verified(document, run_id, hash, registry, &aliases, starter)?;
        Ok(Self {
            world,
            snapshot,
            setup,
        })
    }

    pub fn prepare(&self, template: &Path) -> Result<(), Error> {
        prepare_round(template, &self.world, &self.snapshot, &self.setup)
    }

    pub fn reset(&self) -> Result<PathBuf, Error> {
        reset_round(&self.world, &self.snapshot, &self.setup)
    }
}

pub fn prepare_round(
    template: &Path,
    world: &Path,
    snapshot: &Path,
    setup: &Setup,
) -> Result<(), Error> {
    if world.exists() || snapshot.exists() {
        return Err(Error::Setup(
            "round world or snapshot is already present".into(),
        ));
    }
    let parent = parent(world)?;
    let temporary = tempfile::tempdir_in(parent)?;
    let prepared = temporary.path().join("world");
    clone_world(template, &prepared)?;
    Offline.apply(&prepared, setup)?;
    let snapshot_stage = temporary.path().join("snapshot");
    clone_world(&prepared, &snapshot_stage)?;
    Helper.apply(&prepared, setup)?;
    fs::rename(&snapshot_stage, snapshot)?;
    if let Err(error) = fs::rename(&prepared, world) {
        fs::rename(snapshot, &snapshot_stage)?;
        return Err(error.into());
    }
    Ok(())
}

/// Restore a pristine snapshot and preserve the previous round in a sibling backup directory.
pub fn reset_round(world: &Path, snapshot: &Path, setup: &Setup) -> Result<PathBuf, Error> {
    let parent = parent(world)?;
    let temporary = tempfile::tempdir_in(parent)?;
    let prepared = temporary.path().join("world");
    clone_world(snapshot, &prepared)?;
    Helper.apply(&prepared, setup)?;
    let backup_directory = tempfile::Builder::new()
        .prefix("previous-round-")
        .tempdir_in(parent)?;
    let backup = backup_directory.path().join("world");
    fs::rename(world, &backup)?;
    if let Err(error) = fs::rename(&prepared, world) {
        if let Err(restore) = fs::rename(&backup, world) {
            let backup_directory = backup_directory.keep();
            return Err(Error::Setup(format!(
                "reset failed ({error}); restore failed ({restore}); recover from {}",
                backup_directory.display()
            )));
        }
        return Err(error.into());
    }
    let _ = backup_directory.keep();
    Ok(backup)
}

/// Clone a read-only template without following symlinks or copying the game lock.
pub fn clone_world(template: &Path, destination: &Path) -> Result<(), Error> {
    if !template.join("level.dat").is_file() {
        return Err(Error::Setup(format!(
            "template has no level.dat: {}",
            template.display()
        )));
    }
    if destination.exists() {
        return Err(Error::Setup(format!(
            "world already exists: {}",
            destination.display()
        )));
    }
    let staging = tempfile::tempdir_in(parent(destination)?)?;
    let target = staging.path().join("world");
    copy_tree(template, &target)?;
    fs::rename(&target, destination)?;
    Ok(())
}

fn copy_tree(source: &Path, target: &Path) -> Result<(), Error> {
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let name = entry.file_name();
        if name == "session.lock" {
            continue;
        }
        if kind.is_dir() {
            copy_tree(&entry.path(), &target.join(name))?;
        } else if kind.is_file() {
            let mut output = File::create(target.join(name))?;
            io::copy(&mut File::open(entry.path())?, &mut output)?;
        } else {
            return Err(Error::Setup(format!(
                "template contains an unsupported filesystem entry: {}",
                entry.path().display()
            )));
        }
    }
    Ok(())
}

/// Materialize one of the two original, empty 1.8.9 template saves.
pub fn create_builtin_template(templates: &Path, mode: Mode) -> Result<PathBuf, Error> {
    let (name, options) = match mode {
        Mode::ChallengeGames => (
            "challenge_games",
            "3;minecraft:bedrock,2*minecraft:dirt,minecraft:grass;1;",
        ),
        Mode::LuckyBlockBedwars => ("lucky_block_bedwars", "3;minecraft:air;1;"),
    };
    let path = templates.join(name);
    fs::create_dir_all(templates)?;
    if path.join("level.dat").is_file() {
        return Ok(path);
    }
    let data = HashMap::from([
        ("LevelName".into(), Tag::String(name.into())),
        ("generatorName".into(), Tag::String("flat".into())),
        ("generatorVersion".into(), Tag::Int(0)),
        ("generatorOptions".into(), Tag::String(options.into())),
        ("RandomSeed".into(), Tag::Long(942493001)),
        ("GameType".into(), Tag::Int(0)),
        ("version".into(), Tag::Int(19133)),
        ("initialized".into(), Tag::Byte(0)),
        ("allowCommands".into(), Tag::Byte(0)),
        ("MapFeatures".into(), Tag::Byte(0)),
        ("Difficulty".into(), Tag::Byte(2)),
    ]);
    let staging = tempfile::tempdir_in(templates)?;
    let output = staging.path().join("world");
    fs::create_dir(&output)?;
    write_level(
        &output.join("level.dat"),
        &HashMap::from([("Data".into(), Tag::Compound(data))]),
    )?;
    match fs::rename(&output, &path) {
        Ok(()) => Ok(path),
        Err(_) if path.join("level.dat").is_file() => Ok(path),
        Err(error) => Err(error.into()),
    }
}

fn parent(path: &Path) -> Result<&Path, Error> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| Error::Setup("world path requires a parent directory".into()))
}

fn read_level(path: &Path) -> Result<HashMap<String, Tag>, Error> {
    let mut bytes = Vec::new();
    GzDecoder::new(File::open(path)?)
        .take(16 * 1024 * 1024)
        .read_to_end(&mut bytes)?;
    Ok(fastnbt::from_bytes(&bytes)?)
}

fn write_level(path: &Path, value: &HashMap<String, Tag>) -> Result<(), Error> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&fastnbt::to_bytes(value)?)?;
    publish(path, &encoder.finish()?)
}

fn compound<'a>(
    value: &'a mut HashMap<String, Tag>,
    key: &str,
) -> Result<&'a mut HashMap<String, Tag>, Error> {
    match value
        .entry(key.to_owned())
        .or_insert_with(|| Tag::Compound(HashMap::new()))
    {
        Tag::Compound(data) => Ok(data),
        _ => Err(Error::Setup(format!("NBT {key} is not a compound"))),
    }
}

fn publish(path: &Path, bytes: &[u8]) -> Result<(), Error> {
    let mut file = tempfile::NamedTempFile::new_in(parent(path)?)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_applies_and_restores_original() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let template =
            create_builtin_template(&temp.path().join("templates"), Mode::ChallengeGames)?;
        let template_nbt = fs::read(template.join("level.dat"))?;
        let mut doc: Value =
            serde_json::from_str(include_str!("../../../schema/fixtures/v1/fixture-01.json"))?;
        doc["villagers"] = json!([]);
        let document = Document::from_value(doc)?;
        let registry = Registry {
            mod_set_hash: "verified".into(),
            items: BTreeMap::new(),
            blocks: BTreeMap::new(),
        };
        let setup = Setup::verified(
            &document,
            "test-run",
            "verified",
            &registry,
            &BTreeMap::new(),
            true,
        )?;
        let world = temp.path().join("match");
        let snapshot = temp.path().join("snapshot");
        prepare_round(&template, &world, &snapshot, &setup)?;
        assert!(world.join("runner_setup.json").is_file());
        assert!(!snapshot.join("runner_setup.json").exists());
        assert_eq!(fs::read(template.join("level.dat"))?, template_nbt);
        fs::write(world.join("progress"), b"round one")?;
        let backup = reset_round(&world, &snapshot, &setup)?;
        assert!(backup.join("progress").is_file());
        assert!(!world.join("progress").is_file());
        assert!(world.join("runner_setup.json").is_file());
        Ok(())
    }
}

/// Write a multiplayer bookmark while preserving the other entries in servers.dat.
pub fn bookmark_server(instance: &Path, name: &str, address: &str) -> Result<(), Error> {
    let path = instance.join("servers.dat");
    let mut state: HashMap<String, Tag> = if path.is_file() {
        fastnbt::from_bytes(&fs::read(&path)?)?
    } else {
        HashMap::new()
    };
    let servers = state
        .entry("servers".into())
        .or_insert_with(|| Tag::List(Vec::new()));
    let Tag::List(servers) = servers else {
        return Err(Error::Setup(
            "servers.dat has an invalid servers list".into(),
        ));
    };
    if !servers.iter().any(|record| {
        matches!(record, Tag::Compound(fields) if matches!(fields.get("ip"), Some(Tag::String(ip)) if ip == address))
    }) {
        servers.push(Tag::Compound(HashMap::from([
            ("name".into(), Tag::String(name.into())), ("ip".into(), Tag::String(address.into())),
        ])));
    }
    publish(&path, &fastnbt::to_bytes(&state)?)
}
