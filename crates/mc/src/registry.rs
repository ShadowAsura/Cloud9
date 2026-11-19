use crate::Error;
use cloud9_core::Item;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Alias {
    pub id: String,
    pub meta: Option<u32>,
    pub nbt: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub mod_set_hash: String,
    pub items: BTreeMap<String, u32>,
    #[serde(default)]
    pub blocks: BTreeMap<String, u32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Resolution {
    Exact(String),
    Suggestion(String),
    Unresolved,
}

impl Registry {
    pub fn resolve(
        &self,
        item: &Item,
        aliases: &BTreeMap<String, Alias>,
        block: bool,
    ) -> Resolution {
        let registry = if block { &self.blocks } else { &self.items };
        let alias = aliases
            .get(&item.raw_text.to_lowercase())
            .or_else(|| item.id.as_ref().and_then(|id| aliases.get(id)));
        if let Some(alias) = alias {
            return if registry.contains_key(&alias.id) {
                Resolution::Exact(alias.id.clone())
            } else {
                Resolution::Unresolved
            };
        }
        if let Some(id) = &item.id
            && registry.contains_key(id)
        {
            return Resolution::Exact(id.clone());
        }
        if let Some(number) = item.legacy_id
            && let Some((id, _)) = registry.iter().find(|(_, value)| **value == number)
        {
            return Resolution::Exact(id.clone());
        }
        let raw = item.raw_text.to_lowercase().replace(' ', "_");
        registry
            .keys()
            .map(|key| {
                let label = key.split_once(':').map_or(key.as_str(), |(_, tail)| tail);
                (edit_distance(&raw, label), key)
            })
            .filter(|(distance, _)| *distance <= (raw.len() / 4).min(3))
            .min_by_key(|(distance, key)| (*distance, *key))
            .map_or(Resolution::Unresolved, |(_, key)| {
                Resolution::Suggestion(key.clone())
            })
    }

    pub fn check_hash(&self, expected: &str) -> Result<(), Error> {
        if self.mod_set_hash != expected {
            return Err(Error::Config(
                "registry dump belongs to another mod set".into(),
            ));
        }
        Ok(())
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<_> = right.chars().collect();
    let mut row: Vec<_> = (0..=right.len()).collect();
    for (index, left) in left.chars().enumerate() {
        let mut previous = row[0];
        row[0] = index + 1;
        for (column, right) in right.iter().enumerate() {
            let cost = usize::from(left != *right);
            let current = row[column + 1];
            row[column + 1] = (previous + cost).min(current + 1).min(row[column] + 1);
            previous = current;
        }
    }
    row[right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_precedes_registry_and_fuzzy_results_require_review() -> Result<(), serde_json::Error> {
        let registry = Registry {
            mod_set_hash: "test".into(),
            items: BTreeMap::from([
                ("minecraft:iron_sword".into(), 267),
                ("minecraft:golden_sword".into(), 283),
            ]),
            blocks: BTreeMap::new(),
        };
        let item: Item = serde_json::from_value(serde_json::json!({
            "id": "old:sword", "legacy_id": 267, "raw_text": "gilded sword"
        }))?;
        let aliases = BTreeMap::from([(
            "gilded sword".into(),
            Alias {
                id: "minecraft:golden_sword".into(),
                meta: None,
                nbt: None,
            },
        )]);
        assert_eq!(
            registry.resolve(&item, &aliases, false),
            Resolution::Exact("minecraft:golden_sword".into())
        );
        assert_eq!(
            registry.resolve(&item, &BTreeMap::new(), false),
            Resolution::Exact("minecraft:iron_sword".into())
        );
        let typo: Item = serde_json::from_value(serde_json::json!({"raw_text": "iron swrd"}))?;
        assert_eq!(
            registry.resolve(&typo, &aliases, false),
            Resolution::Suggestion("minecraft:iron_sword".into())
        );
        Ok(())
    }
}
