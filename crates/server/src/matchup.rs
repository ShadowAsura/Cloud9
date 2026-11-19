use crate::{Error, Server, json_write, status};
use cloud9_core::Document;
use cloud9_mc::LockedMod;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashSet, fs, net::IpAddr, path::Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Matchup {
    pub schema_version: u32,
    pub manifest_id: String,
    pub address: String,
    pub mods: Vec<LockedMod>,
}

impl Matchup {
    pub fn from_server(server: &Server) -> Result<Self, Error> {
        let document = Document::load(&server.root.join("episode.json"))?;
        let address = status(&server.root)
            .ok()
            .filter(|status| status.phase == "Running")
            .map_or_else(|| server.address.to_string(), |status| status.address);
        Ok(Self {
            schema_version: 1,
            manifest_id: document.manifest().episode.video_id.clone(),
            address,
            mods: server
                .mods
                .iter()
                .filter(|record| record.mod_id != "runner-helper")
                .cloned()
                .collect(),
        })
    }

    pub fn load(path: &Path) -> Result<Self, Error> {
        if fs::metadata(path)?.len() > 1024 * 1024 {
            return Err(Error::Config("matchup exceeds 1 MiB".into()));
        }
        let matchup: Self = serde_json::from_slice(&fs::read(path)?)?;
        matchup.validate()?;
        Ok(matchup)
    }

    pub fn save(&self, path: &Path) -> Result<(), Error> {
        self.validate()?;
        json_write(path, self)
    }

    fn validate(&self) -> Result<(), Error> {
        if self.schema_version != 1
            || self.manifest_id.is_empty()
            || self.manifest_id.len() > 128
            || !self
                .manifest_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(Error::Config(
                "invalid matchup format or manifest ID".into(),
            ));
        }
        self.host_port()?;
        let mut ids = HashSet::new();
        for jar in &self.mods {
            if jar.entry_id.is_empty()
                || jar.mod_id.is_empty()
                || jar.version.is_empty()
                || jar.sha256.len() != 64
                || !jar.sha256.bytes().all(|b| b.is_ascii_hexdigit())
                || !ids.insert(&jar.entry_id)
                || jar.client_only
                || jar.mod_id == "runner-helper"
                || jar
                    .source
                    .as_ref()
                    .is_some_and(|source| !source.starts_with("https://"))
            {
                return Err(Error::Config("invalid mod lock entry in matchup".into()));
            }
        }
        Ok(())
    }

    pub fn host_port(&self) -> Result<(&str, u16), Error> {
        parse_address(&self.address)
    }

    /// Pin server-side mods by stable manifest record ID before resolving local jars.
    pub fn pin(&self, document: &Document) -> Result<Document, Error> {
        self.validate()?;
        if self.manifest_id != document.manifest().episode.video_id {
            return Err(Error::Config(
                "matchup requires a different episode manifest".into(),
            ));
        }
        let mut source = document.source().clone();
        let records = source["mods"]
            .as_array_mut()
            .ok_or_else(|| Error::Config("manifest has no mod array".into()))?;
        for locked in &self.mods {
            let record = records
                .iter_mut()
                .find(|record| record["id"] == locked.entry_id)
                .ok_or_else(|| {
                    Error::Config(format!(
                        "matchup mod {} is absent from the episode manifest",
                        locked.entry_id
                    ))
                })?;
            record["mod_id"] = json!({"value":locked.mod_id,"confidence":1,"source":"manual","status":"overridden"});
            record["version_range"] = json!({"value":locked.version,"confidence":1,"source":"manual","status":"overridden"});
            if !record["file_hint"].is_object() {
                record["file_hint"] = json!({});
            }
            record["file_hint"]["sha256"] = json!(locked.sha256);
            if let Some(url) = &locked.source {
                record["file_hint"]["url"] = json!(url);
            }
            record["optional"] = json!(false);
            record["client_only"] = json!(false);
        }
        if records.iter().any(|record| {
            record["optional"] == false
                && record["client_only"] == false
                && !self
                    .mods
                    .iter()
                    .any(|locked| record["id"] == locked.entry_id)
        }) {
            return Err(Error::Config("matchup omits a required server mod".into()));
        }
        records.retain(|record| {
            record["client_only"] == true
                || self
                    .mods
                    .iter()
                    .any(|locked| record["id"] == locked.entry_id)
        });
        Ok(Document::from_value(source)?)
    }
}

pub(crate) fn parse_address(address: &str) -> Result<(&str, u16), Error> {
    let fail = || Error::Config("join address must be a hostname or IP with a port".into());
    let (host, port) = address.rsplit_once(':').ok_or_else(fail)?;
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or_else(fail)?;
    if let Some(bracketed) = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
    {
        if bracketed.parse::<std::net::Ipv6Addr>().is_ok() {
            return Ok((bracketed, port));
        }
        return Err(fail());
    }
    if host.parse::<IpAddr>().is_ok_and(|ip| ip.is_ipv4())
        || (host.len() <= 253
            && host.split('.').all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && !label.starts_with('-')
                    && !label.ends_with('-')
                    && label
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            }))
    {
        Ok((host, port))
    } else {
        Err(fail())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn share_round_trip_pins_hash_by_manifest_id() -> Result<(), Box<dyn std::error::Error>> {
        let document = Document::from_value(serde_json::from_str(include_str!(
            "../../../schema/fixtures/v1/fixture-01.json"
        ))?)?;
        let matchup = Matchup {
            schema_version: 1,
            manifest_id: "fixture-01".into(),
            address: "[::1]:25565".into(),
            mods: vec![LockedMod {
                entry_id: "spiral".into(),
                mod_id: "spiralmod".into(),
                version: "1.2".into(),
                sha256: "a".repeat(64),
                source: None,
                client_only: false,
            }],
        };
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("round.matchup");
        matchup.save(&path)?;
        let imported = Matchup::load(&path)?;
        assert_eq!(imported.host_port()?, ("::1", 25565));
        let pinned = imported.pin(&document)?;
        assert_eq!(
            pinned.manifest().mods[0].mod_id.value.as_deref(),
            Some("spiralmod")
        );
        assert_eq!(
            pinned.manifest().mods[0]
                .file_hint
                .as_ref()
                .and_then(|file| file.sha256.as_deref()),
            Some("a".repeat(64).as_str())
        );
        assert!(parse_address("https://bad.example:25565").is_err());
        Ok(())
    }
}
