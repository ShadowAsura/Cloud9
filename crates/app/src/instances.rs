use crate::{ACCENT, BORDER, MUTED, PANEL};
use cloud9_mc::Instance;
use eframe::egui::{self, RichText, Stroke};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

struct Card {
    title: String,
    path: PathBuf,
    profile: String,
    state: &'static str,
    changed: SystemTime,
}

#[derive(Default)]
pub struct Instances {
    cards: Vec<Card>,
    refreshed: Option<Instant>,
    error: Option<String>,
}

impl Instances {
    fn reload(&mut self) {
        self.refreshed = Some(Instant::now());
        match scan(Path::new("instances")) {
            Ok(cards) => {
                self.cards = cards;
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    pub fn view(&mut self, ui: &mut egui::Ui) -> Option<PathBuf> {
        if self
            .refreshed
            .is_none_or(|time| time.elapsed() > Duration::from_secs(3))
        {
            self.reload();
        }
        ui.horizontal(|ui| {
            ui.heading(RichText::new("Instances").size(31.0));
            if ui.button("Reload").clicked() {
                self.reload();
            }
        });
        ui.add_space(8.0);
        if let Some(error) = &self.error {
            ui.colored_label(crate::ERROR, error);
            return None;
        }
        if self.cards.is_empty() {
            ui.colored_label(
                MUTED,
                "Choose an episode and launch it to create a saved instance.",
            );
        }
        let mut selection = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for card in &self.cards {
                egui::Frame::new()
                    .fill(PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(2)
                    .inner_margin(12)
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&card.title).size(18.0));
                                ui.label(RichText::new(&card.profile).monospace().color(MUTED));
                                ui.colored_label(ACCENT, card.state);
                            });
                            if ui.button("Open").clicked() {
                                selection = Some(card.path.clone());
                            }
                        });
                    });
            }
        });
        selection
    }
}

fn scan(root: &Path) -> std::io::Result<Vec<Card>> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut cards = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with(".staging-")
            || !entry.file_type()?.is_dir()
            || !path.join("runner.lock.json").is_file()
            || !path.join("episode.json").is_file()
        {
            continue;
        }
        let document = fs::read(path.join("episode.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
        let title = document
            .as_ref()
            .and_then(|doc| doc["episode"]["title"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| entry.file_name().to_string_lossy().into_owned());
        let lock = Instance::inspect(&path);
        let state = if lock.is_err() || document.is_none() {
            "Needs repair"
        } else if path.join("round-snapshot/level.dat").is_file() {
            "Solo save"
        } else if path.join("servers.dat").is_file() {
            "Bookmarked server"
        } else {
            "Setup pending"
        };
        let profile = lock.map_or_else(
            |_| "Instance lock unreadable".into(),
            |lock| {
                format!(
                    "Minecraft {} / Forge {} / {} locked mods",
                    lock.profile.mc_version,
                    lock.profile.loader_version,
                    lock.mods.len()
                )
            },
        );
        cards.push(Card {
            title,
            path,
            profile,
            state,
            changed: entry
                .metadata()?
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
    cards.sort_by(|left, right| {
        right
            .changed
            .cmp(&left.changed)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(cards)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_published_instances_and_excludes_staging() -> std::io::Result<()> {
        let root = tempfile::tempdir()?;
        for name in ["saved", ".staging-test"] {
            let path = root.path().join(name);
            fs::create_dir(&path)?;
            fs::write(
                path.join("episode.json"),
                br#"{"episode":{"title":"Round"}}"#,
            )?;
            fs::write(path.join("runner.lock.json"), b"invalid")?;
        }
        let cards = scan(root.path())?;
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "Round");
        assert_eq!(cards[0].state, "Needs repair");
        Ok(())
    }
}
