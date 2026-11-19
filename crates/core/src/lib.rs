use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{BufReader, Write},
    path::Path,
    sync::OnceLock,
};
use thiserror::Error;

pub const SCHEMA_VERSION: u64 = 1;
const MANIFEST_SCHEMA: &str = include_str!("../../../schema/manifest-v1.schema.json");
const OVERRIDE_SCHEMA: &str = include_str!("../../../schema/overrides-v1.schema.json");
static MANIFEST_VALIDATOR: OnceLock<Result<jsonschema::Validator, String>> = OnceLock::new();
static OVERRIDE_VALIDATOR: OnceLock<Result<jsonschema::Validator, String>> = OnceLock::new();
type Extensions = BTreeMap<String, Value>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("unsupported manifest schema version {0}")]
    Version(u64),
    #[error("invalid document at {path}: {message}")]
    Invalid { path: String, message: String },
}

fn invalid(path: &str, message: impl Into<String>) -> Error {
    Error::Invalid {
        path: path.to_owned(),
        message: message.into(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence<T> {
    pub value: T,
    pub confidence: f64,
    pub source: String,
    pub status: Status,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Auto,
    NeedsReview,
    Confirmed,
    Overridden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    ChallengeGames,
    LuckyBlockBedwars,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Episode {
    pub video_id: String,
    pub title: String,
    pub url: String,
    pub upload_date: Option<String>,
    pub duration: Option<u64>,
    pub thumbnail_path: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Loader {
    #[serde(rename = "type")]
    pub kind: Evidence<String>,
    pub version: Evidence<Option<String>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub mc_version: Evidence<String>,
    pub loader: Loader,
    pub java_major: Evidence<Option<u32>>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModRole {
    Lucky,
    Companion,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileHint {
    pub filename: Option<String>,
    pub sha256: Option<String>,
    pub url: Option<String>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mod {
    pub id: String,
    pub role: ModRole,
    pub mod_id: Evidence<Option<String>>,
    pub display_name: Evidence<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version_range: Option<Evidence<Option<String>>>,
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_hint: Option<FileHint>,
    pub optional: bool,
    pub client_only: bool,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Map {
    pub template_id: String,
    pub overrides: Extensions,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub legacy_id: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nbt: Option<Extensions>,
    pub raw_text: String,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stack {
    pub id: String,
    pub item: Item,
    pub count: u32,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trade {
    pub id: String,
    pub buy: Vec<Stack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buy_b: Option<Stack>,
    pub sell: Stack,
    pub max_uses: u32,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Villager {
    pub id: String,
    pub role: String,
    pub position: [i32; 3],
    pub trades: Vec<Trade>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chest {
    pub id: String,
    pub pos: [i32; 3],
    pub slots: Vec<ChestSlot>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChestSlot {
    pub id: String,
    pub slot: u8,
    pub item: Item,
    pub count: u32,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub id: String,
    pub pos: [i32; 3],
    pub item: Item,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generator {
    pub id: String,
    pub pos: [i32; 3],
    pub item: Item,
    pub interval_ticks: u32,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Center {
    pub kind: String,
    pub blocks: Vec<Block>,
    pub chests: Vec<Chest>,
    pub generators: Vec<Generator>,
    #[serde(flatten)]
    pub extensions: Extensions,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: u64,
    pub episode: Episode,
    pub mode: Evidence<Mode>,
    pub game: Game,
    pub mods: Vec<Mod>,
    pub map: Map,
    pub villagers: Vec<Villager>,
    pub center: Center,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub allow_multiple_lucky: bool,
    #[serde(flatten)]
    pub extensions: Extensions,
}

/// Validated source document and its typed projection. The source retains explicit
/// nulls and unknown fields for lossless round trips.
#[derive(Debug, Clone)]
pub struct Document {
    source: Value,
    manifest: Manifest,
}

impl Document {
    pub fn load(path: &Path) -> Result<Self, Error> {
        Self::from_value(serde_json::from_reader(BufReader::new(File::open(path)?))?)
    }

    pub fn from_value(source: Value) -> Result<Self, Error> {
        let source = migrate(source)?;
        validate(&source, MANIFEST_SCHEMA, &MANIFEST_VALIDATOR)?;
        validate_stable_ids(&source, "")?;
        let manifest = serde_json::from_value(source.clone())?;
        Ok(Self { source, manifest })
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn source(&self) -> &Value {
        &self.source
    }

    pub fn save(&self, path: &Path) -> Result<(), Error> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        let mut file = tempfile::NamedTempFile::new_in(parent.unwrap_or(Path::new(".")))?;
        serde_json::to_writer_pretty(&mut file, &self.source)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|err| err.error)?;
        Ok(())
    }
}

pub(crate) fn validate(
    source: &Value,
    schema: &str,
    cache: &OnceLock<Result<jsonschema::Validator, String>>,
) -> Result<(), Error> {
    let result = cache.get_or_init(|| {
        let spec = serde_json::from_str(schema).map_err(|err| err.to_string())?;
        jsonschema::validator_for(&spec).map_err(|err| err.to_string())
    });
    let validator = result.as_ref().map_err(|err| invalid("", err))?;
    validator
        .validate(source)
        .map_err(|err| invalid(&err.instance_path.to_string(), err.to_string()))
}

/// Dispatch version-specific upgrades before validation. Version 1 is the first
/// published format, and future versions must register explicit upgrade steps.
pub fn migrate(source: Value) -> Result<Value, Error> {
    let version = source
        .get("schema_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("/schema_version", "expected an unsigned version number"))?;
    match version {
        SCHEMA_VERSION => Ok(source),
        _ => Err(Error::Version(version)),
    }
}

fn validate_stable_ids(value: &Value, path: &str) -> Result<(), Error> {
    match value {
        Value::Array(entries) => {
            let mut ids = HashSet::new();
            for (index, entry) in entries.iter().enumerate() {
                if let Some(id) = entry.get("id").and_then(Value::as_str) {
                    if !ids.insert(id) {
                        return Err(invalid(path, format!("duplicate stable id {id}")));
                    }
                }
                validate_stable_ids(entry, &format!("{path}/{index}"))?;
            }
        }
        Value::Object(fields) => {
            for (key, value) in fields {
                if matches!(key.as_str(), "nbt" | "overrides") {
                    continue;
                }
                validate_stable_ids(value, &format!("{path}/{key}"))?;
            }
        }
        _ => {}
    }
    Ok(())
}

mod review;
pub use review::{Conflict, apply_overrides, merge_overrides};
