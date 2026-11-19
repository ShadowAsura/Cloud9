mod instances;
mod runner;
mod servers;

use cloud9_core::catalog::{Catalog, EpisodeCard, Filters};
use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, FontData, FontFamily, FontId, Frame, RichText, Stroke,
    TextStyle, TextureHandle, TextureOptions, pos2, vec2,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, SyncSender, channel, sync_channel},
};

const GROUND: Color32 = Color32::from_rgb(22, 20, 14);
const PANEL: Color32 = Color32::from_rgb(29, 26, 17);
const TEXT: Color32 = Color32::from_rgb(232, 226, 208);
const MUTED: Color32 = Color32::from_rgb(163, 156, 128);
const ACCENT: Color32 = Color32::from_rgb(242, 194, 48);
const OK: Color32 = Color32::from_rgb(143, 192, 122);
const ERROR: Color32 = Color32::from_rgb(238, 122, 103);
const BORDER: Color32 = Color32::from_rgb(46, 42, 27);
const OUTLINE: Color32 = Color32::from_rgb(58, 53, 34);

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/episodes.sqlite"));
    let episode = args.next().and_then(|id| id.into_string().ok());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([740.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Cloud 9",
        options,
        Box::new(move |creation| {
            set_theme(&creation.egui_ctx);
            let mut browser = Browser::new(path, creation.egui_ctx.clone());
            if let Some(id) = episode {
                browser.select_episode(&id);
            }
            Ok(Box::new(browser))
        }),
    )
}

fn set_theme(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, family, bytes) in [
        (
            "plex-sans",
            FontFamily::Proportional,
            include_bytes!("../fonts/IBMPlexSans-Regular.ttf").as_slice(),
        ),
        (
            "plex-mono",
            FontFamily::Monospace,
            include_bytes!("../fonts/IBMPlexMono-Regular.ttf").as_slice(),
        ),
    ] {
        fonts
            .font_data
            .insert(name.into(), FontData::from_static(bytes).into());
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, name.into());
    }
    ctx.set_fonts(fonts);
    ctx.style_mut(|style| {
        style.spacing.item_spacing = vec2(10.0, 8.0);
        style.spacing.button_padding = vec2(12.0, 7.0);
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(15.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(15.0));
        let visual = &mut style.visuals;
        visual.panel_fill = GROUND;
        visual.window_fill = PANEL;
        visual.extreme_bg_color = PANEL;
        visual.override_text_color = Some(TEXT);
        visual.hyperlink_color = ACCENT;
        visual.window_corner_radius = CornerRadius::same(2);
        visual.window_shadow = egui::Shadow::NONE;
        visual.popup_shadow = egui::Shadow::NONE;
        visual.selection.bg_fill = OUTLINE;
        for widget in [
            &mut visual.widgets.noninteractive,
            &mut visual.widgets.inactive,
            &mut visual.widgets.hovered,
            &mut visual.widgets.active,
            &mut visual.widgets.open,
        ] {
            widget.corner_radius = CornerRadius::same(2);
            widget.bg_fill = PANEL;
            widget.weak_bg_fill = PANEL;
            widget.bg_stroke = Stroke::new(1.0_f32, OUTLINE);
            widget.fg_stroke = Stroke::new(1.0_f32, TEXT);
        }
        visual.widgets.hovered.bg_stroke.color = ACCENT;
        visual.widgets.active.bg_stroke.color = ACCENT;
    });
}

struct Thumbnails {
    send: SyncSender<String>,
    receive: Receiver<(String, Option<ColorImage>)>,
    pending: HashSet<String>,
    images: HashMap<String, Option<TextureHandle>>,
}

impl Thumbnails {
    fn new(ctx: egui::Context) -> Self {
        let (send, queue) = sync_channel::<String>(12);
        let (output, receive) = channel();
        std::thread::spawn(move || {
            while let Ok(path) = queue.recv() {
                let result = decode_thumbnail(Path::new(&path));
                if output.send((path, result)).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self {
            send,
            receive,
            pending: HashSet::new(),
            images: HashMap::new(),
        }
    }

    fn update(&mut self, ctx: &egui::Context) {
        for (path, result) in self.receive.try_iter() {
            self.pending.remove(&path);
            if self.images.len() >= 128 {
                self.images.clear();
            }
            let texture =
                result.map(|image| ctx.load_texture(&path, image, TextureOptions::LINEAR));
            self.images.insert(path, texture);
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, card: &EpisodeCard, width: f32) -> egui::Response {
        let (rect, response) =
            ui.allocate_exact_size(vec2(width, width * 9.0 / 16.0), egui::Sense::click());
        let path = card.thumbnail_path.as_deref();
        let painter = ui.painter();
        painter.rect_filled(rect, 2, GROUND);
        if let Some(path) = path {
            if let Some(Some(texture)) = self.images.get(path) {
                painter.image(
                    texture.id(),
                    rect,
                    thumbnail_uv(texture.aspect_ratio(), rect.aspect_ratio()),
                    Color32::WHITE,
                );
            }
            if !self.images.contains_key(path)
                && !self.pending.contains(path)
                && self.send.try_send(path.to_owned()).is_ok()
            {
                self.pending.insert(path.to_owned());
            }
        }
        if !path.is_some_and(|path| matches!(self.images.get(path), Some(Some(_)))) {
            let label = if path.is_some_and(|path| self.pending.contains(path)) {
                "Loading thumbnail"
            } else {
                "No thumbnail"
            };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                label,
                FontId::monospace(12.0),
                MUTED,
            );
        }
        if let Some(seconds) = card.duration {
            let text = if seconds >= 3600 {
                format!(
                    "{}:{:02}:{:02}",
                    seconds / 3600,
                    seconds / 60 % 60,
                    seconds % 60
                )
            } else {
                format!("{}:{:02}", seconds / 60, seconds % 60)
            };
            let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), TEXT);
            let badge = egui::Rect::from_min_size(
                rect.max - vec2(8.0, 8.0) - galley.size() - vec2(10.0, 6.0),
                galley.size() + vec2(10.0, 6.0),
            );
            painter.rect_filled(badge, 2, Color32::from_black_alpha(215));
            painter.galley(badge.min + vec2(5.0, 3.0), galley, TEXT);
        }
        if response.hovered() {
            painter.rect_stroke(
                rect,
                2,
                Stroke::new(1.0_f32, ACCENT),
                egui::StrokeKind::Inside,
            );
        }
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(&card.title)
    }
}

fn thumbnail_uv(source: f32, destination: f32) -> egui::Rect {
    let inset_x = (1.0 - destination / source).max(0.0) / 2.0;
    let inset_y = (1.0 - source / destination).max(0.0) / 2.0;
    egui::Rect::from_min_max(pos2(inset_x, inset_y), pos2(1.0 - inset_x, 1.0 - inset_y))
}

fn decode_thumbnail(path: &Path) -> Option<ColorImage> {
    let mut reader = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().ok()?.thumbnail(512, 384).to_rgba8();
    Some(ColorImage::from_rgba_unmultiplied(
        [image.width() as usize, image.height() as usize],
        &image,
    ))
}

#[derive(PartialEq, Eq)]
enum Screen {
    Episodes,
    Instances,
    Running,
    Servers,
}

struct Browser {
    screen: Screen,
    play_dialog: bool,
    runner: runner::Runner,
    instances: instances::Instances,
    servers: servers::Servers,
    path: PathBuf,
    catalog: Option<Catalog>,
    catalog_version: Option<u64>,
    next_catalog_check: std::time::Instant,
    filters: Filters,
    cards: Vec<EpisodeCard>,
    error: Option<String>,
    page: u32,
    next: bool,
    reset_scroll: bool,
    thumbs: Thumbnails,
}

impl Browser {
    fn select_episode(&mut self, id: &str) {
        let document = self
            .catalog
            .as_ref()
            .ok_or_else(|| "Catalog unavailable".into())
            .and_then(|catalog| catalog.document(id));
        self.runner.select(document);
        self.play_dialog = true;
    }

    fn new(path: PathBuf, ctx: egui::Context) -> Self {
        let mut browser = Self {
            screen: Screen::Episodes,
            play_dialog: false,
            runner: runner::Runner::new(),
            instances: instances::Instances::default(),
            servers: servers::Servers::new(),
            path,
            catalog: None,
            catalog_version: None,
            next_catalog_check: std::time::Instant::now(),
            filters: Filters::default(),
            cards: Vec::new(),
            error: None,
            page: 0,
            next: false,
            reset_scroll: true,
            thumbs: Thumbnails::new(ctx),
        };
        browser.open();
        browser
    }

    fn open(&mut self) {
        match Catalog::open(&self.path) {
            Ok(catalog) => {
                self.catalog_version = catalog.data_version().ok();
                self.catalog = Some(catalog);
                self.refresh();
            }
            Err(error) => {
                self.catalog = None;
                self.error = Some(format!("Could not open {} ({error})", self.path.display()));
            }
        }
    }

    fn check_catalog(&mut self, ctx: &egui::Context) {
        let interval = std::time::Duration::from_secs(2);
        let now = std::time::Instant::now();
        if now >= self.next_catalog_check {
            self.next_catalog_check = now + interval;
            let version = self
                .catalog
                .as_ref()
                .and_then(|catalog| catalog.data_version().ok());
            if version.is_some() && version != self.catalog_version {
                self.catalog_version = version;
                let reset_scroll = self.reset_scroll;
                self.refresh();
                self.reset_scroll = reset_scroll;
            }
        }
        ctx.request_repaint_after(interval);
    }

    fn refresh(&mut self) {
        self.reset_scroll = true;
        if let Some(catalog) = &self.catalog {
            match catalog.page(&self.filters, self.page) {
                Ok((cards, next)) => {
                    self.cards = cards;
                    self.next = next;
                    self.error = None;
                }
                Err(error) => self.error = Some(format!("Episode query failed ({error})")),
            }
        }
    }
}

impl eframe::App for Browser {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.screen == Screen::Episodes {
            self.check_catalog(ctx);
        }
        self.thumbs.update(ctx);
        if self.runner.poll(ctx) {
            self.screen = Screen::Running;
            self.play_dialog = false;
        }
        self.servers.poll(ctx);
        egui::TopBottomPanel::top("header")
            .frame(
                Frame::new()
                    .fill(PANEL)
                    .inner_margin(egui::Margin::symmetric(24, 14))
                    .stroke(Stroke::new(1.0_f32, BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("CLOUD 9")
                            .monospace()
                            .strong()
                            .color(ACCENT)
                            .size(19.0),
                    );
                    ui.separator();
                    for (screen, label) in [
                        (Screen::Episodes, "Episodes"),
                        (Screen::Instances, "Instances"),
                        (Screen::Running, "Running"),
                        (Screen::Servers, "Servers"),
                    ] {
                        ui.selectable_value(&mut self.screen, screen, label);
                    }
                });
            });
        egui::CentralPanel::default()
            .frame(Frame::new().fill(GROUND).inner_margin(24))
            .show(ctx, |ui| {
                match self.screen {
                    Screen::Instances => {
                        if let Some(path) = self.instances.view(ui) {
                            self.runner.select_saved(path);
                            self.play_dialog = true;
                        }
                        return;
                    }
                    Screen::Running => {
                        if self.runner.running(ui) {
                            self.play_dialog = true;
                        }
                        return;
                    }
                    Screen::Servers => {
                        if let Some(path) = self.servers.view(ui, &self.runner) {
                            let imported = (|| -> Result<_, String> {
                                let matchup = cloud9_server::Matchup::load(&path).map_err(|error| error.to_string())?;
                                let document = self.catalog.as_ref()
                                    .ok_or_else(|| "Open an episode catalog before importing a matchup".to_owned())?
                                    .document(&matchup.manifest_id)
                                    .map_err(|_| format!("Add episode {} to the catalog before importing this matchup", matchup.manifest_id))?;
                                Ok((matchup.pin(&document).map_err(|error| error.to_string())?, matchup))
                            })();
                            self.runner.import(imported);
                            self.play_dialog = true;
                        }
                        return;
                    }
                    Screen::Episodes => {}
                }
                ui.horizontal(|ui| {
                    ui.heading(RichText::new("Episodes").size(31.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Reload index").clicked() {
                            self.open();
                        }
                    });
                });
                ui.add_space(12.0);
                let mut changed = ui
                    .add_sized(
                        [ui.available_width(), 36.0],
                        egui::TextEdit::singleline(&mut self.filters.search)
                            .hint_text("Search episode titles or IDs"),
                    )
                    .changed();
                ui.horizontal(|ui| {
                    let previous = self.filters.mode.clone();
                    egui::ComboBox::from_id_salt("mode")
                        .selected_text(mode_label(&self.filters.mode))
                        .show_ui(ui, |ui| {
                            for mode in ["", "challenge_games", "lucky_block_bedwars"] {
                                ui.selectable_value(
                                    &mut self.filters.mode,
                                    mode.to_owned(),
                                    mode_label(mode),
                                );
                            }
                        });
                    changed |= self.filters.mode != previous;
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.filters.tag)
                                .desired_width(165.0)
                                .hint_text("Addon or mob"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.filters.version)
                                .desired_width(110.0)
                                .hint_text("MC version"),
                        )
                        .changed();
                    if ui.button("Clear filters").clicked() {
                        self.filters = Filters::default();
                        changed = true;
                    }
                });
                if changed {
                    self.page = 0;
                    self.refresh();
                }
                ui.add_space(8.0);
                if let Some(error) = &self.error {
                    ui.colored_label(ERROR, error);
                    ui.label("Create the SQLite index with the scraper, then reload.");
                    return;
                }
                if self.cards.is_empty() {
                    ui.colored_label(MUTED, "No episodes match these filters.");
                    return;
                }
                egui::TopBottomPanel::bottom("paging").show_inside(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(self.page > 0, egui::Button::new("Previous"))
                            .clicked()
                        {
                            self.page -= 1;
                            self.refresh();
                        }
                        ui.label(
                            RichText::new(format!("Page {}", self.page + 1))
                                .monospace()
                                .color(MUTED),
                        );
                        if ui
                            .add_enabled(self.next, egui::Button::new("Next"))
                            .clicked()
                        {
                            self.page += 1;
                            self.refresh();
                        }
                    });
                });
                let mut scroll = egui::ScrollArea::vertical().auto_shrink([false, false]);
                if std::mem::take(&mut self.reset_scroll) {
                    scroll = scroll.vertical_scroll_offset(0.0);
                }
                let gap = 14.0;
                let width = ui.available_width() - 12.0;
                let columns = gallery_columns(width, gap);
                let card_width = (width - gap * (columns - 1) as f32) / columns as f32;
                let row_height = card_width * 9.0 / 16.0 + 144.0;
                let mut selected = None;
                scroll.show_rows(ui, row_height, self.cards.len().div_ceil(columns), |ui, rows| {
                    for row in rows {
                        let start = row * columns;
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            ui.set_min_height(row_height);
                            for card in self.cards[start..(start + columns).min(self.cards.len())].iter() {
                                if render_card(ui, card, &mut self.thumbs, card_width) {
                                    selected = Some(card.video_id.clone());
                                }
                            }
                        });
                    }
                });
                if let Some(id) = selected {
                    self.select_episode(&id);
                }
            });
        if self.play_dialog {
            let dialog = egui::Modal::new(egui::Id::new("play-episode")).show(ctx, |ui| {
                ui.set_width(492.0_f32.min(ctx.screen_rect().width() - 48.0));
                let close = ui
                    .horizontal(|ui| {
                        ui.heading("Episode setup");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.small_button("Close").clicked()
                        })
                        .inner
                    })
                    .inner;
                egui::ScrollArea::vertical()
                    .max_height((ctx.screen_rect().height() - 120.0).max(100.0))
                    .show(ui, |ui| self.runner.plan(ui));
                close
            });
            if dialog.inner || dialog.should_close() {
                self.play_dialog = false;
            }
        }
    }
}

fn mode_label(mode: &str) -> &str {
    match mode {
        "challenge_games" => "Challenge Games",
        "lucky_block_bedwars" => "Lucky Block Bedwars",
        "" => "All modes",
        other => other,
    }
}

fn gallery_columns(width: f32, gap: f32) -> usize {
    (((width + gap) / (248.0 + gap)).floor() as usize).clamp(1, 4)
}

fn render_card(
    ui: &mut egui::Ui,
    card: &EpisodeCard,
    thumbnails: &mut Thumbnails,
    width: f32,
) -> bool {
    Frame::new()
        .fill(PANEL)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(2)
        .show(ui, |ui| {
            let width = width - 2.0;
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            let opened = thumbnails.draw(ui, card, width).clicked();
            Frame::new()
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    let width = width - 20.0;
                    ui.set_width(width);
                    ui.spacing_mut().item_spacing.y = 5.0;
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new(mode_label(&card.mode))
                                .color(ACCENT)
                                .size(11.0),
                        );
                        ui.label(
                            RichText::new(&card.version)
                                .monospace()
                                .color(MUTED)
                                .size(11.0),
                        );
                    });
                    let mut title = egui::text::LayoutJob::simple(
                        card.title.clone(),
                        FontId::proportional(17.0),
                        TEXT,
                        width,
                    );
                    title.wrap.max_rows = 2;
                    ui.allocate_ui_with_layout(
                        vec2(width, 44.0),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.label(title).on_hover_text(&card.title);
                        },
                    );
                    let (status, color) = if card.community_setup {
                        ("Community setup".to_owned(), OK)
                    } else if card.review_count > 0 {
                        (format!("Needs review ({})", card.review_count), MUTED)
                    } else {
                        ("Indexed".into(), MUTED)
                    };
                    let tags = if card.tags.is_empty() {
                        status
                    } else {
                        format!("{status}  /  {}", card.tags.join(", "))
                    };
                    ui.add(
                        egui::Label::new(RichText::new(tags).size(12.0).color(color)).truncate(),
                    );
                    let clicked = ui
                        .horizontal(|ui| {
                            let clicked = ui
                                .small_button(if card.community_setup {
                                    "Play"
                                } else {
                                    "Details"
                                })
                                .clicked();
                            ui.hyperlink_to("Watch episode", &card.url);
                            clicked
                        })
                        .inner;
                    opened || clicked
                })
                .inner
        })
        .inner
}

#[cfg(test)]
mod gallery_tests {
    use super::*;

    #[test]
    fn grid_adapts_to_window_width() {
        assert_eq!(gallery_columns(245.0, 14.0), 1);
        assert_eq!(gallery_columns(690.0, 14.0), 2);
        assert_eq!(gallery_columns(1010.0, 14.0), 3);
        assert_eq!(gallery_columns(1600.0, 14.0), 4);
    }

    #[test]
    fn thumbnail_crops_without_stretching() {
        let uv = thumbnail_uv(4.0 / 3.0, 16.0 / 9.0);
        assert_eq!(uv.width(), 1.0);
        assert!((uv.height() - 0.75).abs() < 0.01);
    }
}
