use crate::{ACCENT, BORDER, ERROR, GROUND, MUTED, OK, SURFACE, TEXT_DIM};
use cloud9_mc::Instance;
use eframe::egui::{self, RichText, Stroke, vec2};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

struct Card {
    title: String,
    path: PathBuf,
    profile: String,
    state: InstanceState,
    changed: SystemTime,
}

#[derive(Debug, PartialEq)]
enum InstanceState {
    NeedsRepair,
    SoloSave,
    BookmarkedServer,
    SetupPending,
}

impl InstanceState {
    fn label(&self) -> &'static str {
        match self {
            InstanceState::NeedsRepair => "Needs repair",
            InstanceState::SoloSave => "Solo save",
            InstanceState::BookmarkedServer => "Bookmarked server",
            InstanceState::SetupPending => "Setup pending",
        }
    }

    fn color(&self) -> egui::Color32 {
        match self {
            InstanceState::NeedsRepair => ERROR,
            InstanceState::SoloSave => OK,
            InstanceState::BookmarkedServer => ACCENT,
            InstanceState::SetupPending => MUTED,
        }
    }
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
            ui.heading(RichText::new("Instances").size(30.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Reload").clicked() {
                    self.reload();
                }
            });
        });
        ui.add_space(12.0);

        if let Some(error) = &self.error {
            ui.colored_label(ERROR, error);
            return None;
        }

        if self.cards.is_empty() {
            ui.colored_label(
                MUTED,
                "No saved instances yet. Choose an episode and launch it to create one.",
            );
            return None;
        }

        let mut selection = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            for card in &self.cards {
                egui::Frame::new()
                    .fill(SURFACE)
                    .stroke(Stroke::new(1.0_f32, BORDER))
                    .corner_radius(crate::CARD_RADIUS)
                    .inner_margin(egui::Margin::same(14_i8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.spacing_mut().item_spacing = vec2(10.0_f32, 8.0_f32);

                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&card.title).size(17.0).strong());
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(&card.profile)
                                        .monospace()
                                        .color(TEXT_DIM)
                                        .small(),
                                );
                            });
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("Open")
                                                    .small()
                                                    .strong()
                                                    .color(GROUND),
                                            )
                                            .fill(ACCENT),
                                        )
                                        .clicked()
                                    {
                                        selection = Some(card.path.clone());
                                    }
                                    ui.add_space(8.0);
                                    ui.colored_label(card.state.color(), card.state.label());
                                },
                            );
                        });
                    });
                ui.add_space(10.0);
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
        let document_is_some = document.is_some();
        let state = if lock.is_err() || !document_is_some {
            InstanceState::NeedsRepair
        } else if path.join("round-snapshot/level.dat").is_file() {
            InstanceState::SoloSave
        } else if path.join("servers.dat").is_file() {
            InstanceState::BookmarkedServer
        } else {
            InstanceState::SetupPending
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
        assert_eq!(cards[0].state, InstanceState::NeedsRepair);
        Ok(())
    }
}
