use rusqlite::{Connection, OpenFlags, params};
use std::path::Path;

pub const PAGE_SIZE: u32 = 60;

#[derive(Debug, Default)]
pub struct Filters {
    pub search: String,
    pub mode: String,
    pub tag: String,
    pub version: String,
}

#[derive(Debug)]
pub struct EpisodeCard {
    pub video_id: String,
    pub title: String,
    pub url: String,
    pub thumbnail_path: Option<String>,
    pub duration: Option<u64>,
    pub mode: String,
    pub version: String,
    pub review_count: u32,
    pub community_setup: bool,
    pub tags: Vec<String>,
}

pub struct Catalog(Connection);

impl Catalog {
    pub fn data_version(&self) -> Result<u64, rusqlite::Error> {
        self.0
            .query_row("PRAGMA data_version", [], |row| row.get(0))
    }

    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(std::time::Duration::from_millis(250))?;
        Ok(Self(connection))
    }

    pub fn document(&self, video_id: &str) -> Result<crate::Document, String> {
        let json: String = self
            .0
            .query_row(
                "SELECT manifest_json FROM episodes WHERE video_id = ?1",
                [video_id],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let value = serde_json::from_str(&json).map_err(|error| error.to_string())?;
        crate::Document::from_value(value).map_err(|error| error.to_string())
    }

    /// Query one bounded page. The extra row tells the browser whether to
    /// enable forward navigation without running a separate count every time.
    pub fn page(
        &self,
        filters: &Filters,
        page: u32,
    ) -> Result<(Vec<EpisodeCard>, bool), rusqlite::Error> {
        let mut statement = self.0.prepare_cached(
            "SELECT e.video_id, e.title, e.url, e.thumbnail_path, e.mode, e.mc_version,
                    e.review_count, e.manifest_json, e.duration
             FROM episodes e
             WHERE (?1 = '' OR instr(lower(e.title), lower(?1)) > 0 OR lower(e.video_id) = lower(?1))
               AND (?2 = '' OR e.mode = ?2)
               AND (?3 = '' OR EXISTS (
                    SELECT 1 FROM episode_tags t WHERE t.video_id = e.video_id AND instr(t.tag, lower(?3)) > 0
               ))
               AND (?4 = '' OR e.mc_version = ?4)
             ORDER BY (e.url LIKE 'https://example.invalid/%'), e.upload_date DESC, e.video_id
             LIMIT ?5 OFFSET ?6",
        )?;
        let rows = statement.query_map(
            params![
                filters.search.trim(),
                filters.mode,
                filters.tag.trim(),
                filters.version.trim(),
                PAGE_SIZE + 1,
                u64::from(page) * u64::from(PAGE_SIZE)
            ],
            |row| {
                let document: String = row.get(7)?;
                let source: serde_json::Value =
                    serde_json::from_str(&document).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        )
                    })?;
                let mut tags = Vec::new();
                if let Some(mods) = source["mods"].as_array() {
                    tags.extend(
                        mods.iter()
                            .filter_map(|entry| entry["display_name"]["value"].as_str())
                            .map(str::to_owned),
                    );
                }
                Ok(EpisodeCard {
                    video_id: row.get(0)?,
                    title: row.get(1)?,
                    url: row.get(2)?,
                    thumbnail_path: row.get(3)?,
                    duration: row.get(8)?,
                    mode: row.get(4)?,
                    version: row.get(5)?,
                    review_count: row.get(6)?,
                    community_setup: source["findings"].as_array().is_some_and(|findings| {
                        findings.iter().any(|entry| {
                            entry["id"] == "community-setup"
                                && matches!(
                                    entry["status"].as_str(),
                                    Some("confirmed" | "overridden")
                                )
                        })
                    }),
                    tags,
                })
            },
        )?;
        let mut cards = rows.collect::<Result<Vec<_>, _>>()?;
        let has_next = cards.len() > PAGE_SIZE as usize;
        cards.truncate(PAGE_SIZE as usize);
        Ok((cards, has_next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_and_pages_a_large_catalog() -> Result<(), Box<dyn std::error::Error>> {
        let mut connection = Connection::open_in_memory()?;
        connection.execute_batch(include_str!("../../../schema/migrations/001.sql"))?;
        let transaction = connection.transaction()?;
        for index in 0..5000 {
            let id = format!("ep-{index:04}");
            let mode = if index == 4999 {
                "lucky_block_bedwars"
            } else {
                "challenge_games"
            };
            transaction.execute(
                "INSERT INTO episodes VALUES (?1, ?2, 'https://example.invalid', NULL, NULL, NULL, ?3, '1.8.9', 1, '{}')",
                params![id, format!("Spiral episode {index}"), mode],
            )?;
            transaction.execute("INSERT INTO episode_tags VALUES (?1, 'spiral')", [&id])?;
        }
        transaction.commit()?;
        let catalog = Catalog(connection);
        let (first, next) = catalog.page(&Filters::default(), 0)?;
        assert_eq!(first.len(), PAGE_SIZE as usize);
        assert!(next);
        let (second, _) = catalog.page(&Filters::default(), 1)?;
        assert_ne!(first[0].video_id, second[0].video_id);
        let (filtered, next) = catalog.page(
            &Filters {
                search: "Spiral EPISODE".into(),
                mode: "lucky_block_bedwars".into(),
                tag: "SPIRAL".into(),
                version: "1.8.9".into(),
            },
            0,
        )?;
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].video_id, "ep-4999");
        assert!(!next);
        Ok(())
    }
}
