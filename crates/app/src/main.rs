mod instances;
mod runner;
mod servers;

use cloud9_core::catalog::{Catalog, EpisodeCard, Filters};
use eframe::egui::{
    self, Color32, ColorImage, CornerRadius, FontData, FontFamily, FontId, Frame, RichText,
    Stroke, TextStyle, TextureHandle, TextureOptions, pos2, vec2,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::mpsc::{Receiver, SyncSender, channel, sync_channel},
    time::Instant,
};

// Design tokens — one place to tune the look and feel.
// Made pub(crate) so the other UI modules can share the same palette.
// Dark launcher theme in the spirit of CurseForge Classic: near-black slate,
 // lifted panels, light text, single orange accent.
pub(crate) const GROUND: Color32 = Color32::from_rgb(20, 22, 26);
pub(crate) const PANEL: Color32 = Color32::from_rgb(29, 32, 38);
pub(crate) const SURFACE: Color32 = Color32::from_rgb(38, 43, 51);
pub(crate) const TEXT: Color32 = Color32::from_rgb(237, 239, 242);
pub(crate) const TEXT_DIM: Color32 = Color32::from_rgb(185, 190, 198);
pub(crate) const MUTED: Color32 = Color32::from_rgb(125, 133, 144);
pub(crate) const ACCENT: Color32 = Color32::from_rgb(241, 100, 54);
pub(crate) const ACCENT_FAINT: Color32 = Color32::from_rgb(66, 32, 22);
pub(crate) const OK: Color32 = Color32::from_rgb(143, 192, 122);
pub(crate) const ERROR: Color32 = Color32::from_rgb(230, 110, 90);
pub(crate) const BORDER: Color32 = Color32::from_rgb(52, 58, 68);

pub(crate) const CARD_RADIUS: f32 = 6.0;
pub(crate) const PANEL_RADIUS: u8 = 8;

fn main() -> eframe::Result {
    let mut args = std::env::args_os().skip(1);
    let path = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/episodes.sqlite"));
    let episode = args.next().and_then(|id| id.into_string().ok());
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([1024.0, 600.0]),
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
        style.spacing.item_spacing = vec2(10.0_f32, 8.0_f32);
        style.spacing.button_padding = vec2(14.0_f32, 8.0_f32);
        style.spacing.menu_margin = egui::Margin {
            left: 10_i8,
            right: 10_i8,
            top: 8_i8,
            bottom: 8_i8,
        };
        style
            .text_styles
            .insert(TextStyle::Body, FontId::proportional(15.0));
        style
            .text_styles
            .insert(TextStyle::Button, FontId::proportional(15.0));
        style
            .text_styles
            .insert(TextStyle::Small, FontId::proportional(12.0));
        style
            .text_styles
            .insert(TextStyle::Heading, FontId::proportional(26.0));
        let visual = &mut style.visuals;
        visual.panel_fill = GROUND;
        visual.window_fill = PANEL;
        visual.extreme_bg_color = PANEL;
        visual.override_text_color = Some(TEXT);
        visual.hyperlink_color = ACCENT;
        visual.window_corner_radius = CornerRadius::same(PANEL_RADIUS);
        visual.window_shadow = egui::Shadow::NONE;
        visual.popup_shadow = egui::Shadow::NONE;
        visual.selection.bg_fill = ACCENT;
        for widget in [
            &mut visual.widgets.noninteractive,
            &mut visual.widgets.inactive,
            &mut visual.widgets.hovered,
            &mut visual.widgets.active,
            &mut visual.widgets.open,
        ] {
            widget.corner_radius = CornerRadius::same(4_u8);
            widget.bg_fill = PANEL;
            widget.weak_bg_fill = PANEL;
            widget.bg_stroke = Stroke::new(1.0_f32, BORDER);
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
            let texture = result.map(|image| ctx.load_texture(&path, image, TextureOptions::LINEAR));
            self.images.insert(path, texture);
        }
    }

    fn draw(&mut self, ui: &mut egui::Ui, card: &EpisodeCard, width: f32) -> egui::Response {
        let height = width * 9.0 / 16.0;
        let (rect, response) = ui.allocate_exact_size(vec2(width, height), egui::Sense::click());
        let path = card.thumbnail_path.as_deref();
        let painter = ui.painter();
        painter.rect_filled(rect, 2.0, GROUND);
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
                format!("Loading thumbnail{}", loading_dots())
            } else {
                "No thumbnail".to_owned()
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
                format!("{}:{:02}:{:02}", seconds / 3600, seconds / 60 % 60, seconds % 60)
            } else {
                format!("{}:{:02}", seconds / 60, seconds % 60)
            };
            let galley = painter.layout_no_wrap(text, FontId::monospace(12.0), TEXT);
            let badge = egui::Rect::from_min_size(
                rect.max - vec2(8.0_f32, 8.0_f32) - galley.size() - vec2(10.0_f32, 6.0_f32),
                galley.size() + vec2(10.0_f32, 6.0_f32),
            );
            painter.rect_filled(badge, 2.0, Color32::from_black_alpha(215));
            painter.galley(badge.min + vec2(5.0_f32, 3.0_f32), galley, TEXT);
        }
        if response.hovered() {
            painter.rect_stroke(
                rect,
                2.0_f32,
                Stroke::new(1.5_f32, ACCENT),
                egui::StrokeKind::Inside,
            );
        }
        response
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(&card.title)
    }
}

/// Animated dots for loading states (repaint-driven by the caller).
fn loading_dots() -> String {
    let ticks = (Instant::now().elapsed().as_secs_f64() * 2.0) as usize;
    ".".repeat(ticks % 4)
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
    dialog_alpha: f32,
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
            dialog_alpha: 0.0,
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

    /// Draws a dimmed backdrop behind an open dialog so the open state feels animated.
    fn dialog_backdrop(&self, ctx: &egui::Context) {
        if self.dialog_alpha <= 0.0 {
            return;
        }
        let painter = ctx.layer_painter(eframe::egui::LayerId::background());
        let alpha = (150.0 * self.dialog_alpha.clamp(0.0, 1.0)) as u8;
        painter.rect_filled(ctx.screen_rect(), 0.0, Color32::from_black_alpha(alpha));
    }
}

impl eframe::App for Browser {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if self.screen == Screen::Episodes {
            self.check_catalog(ctx);
        }
        self.thumbs.update(ctx);
        // Keep repainting while thumbnails load so the loading-dots animation runs.
        if !self.thumbs.pending.is_empty() {
            ctx.request_repaint();
        }
        if self.runner.poll(ctx) {
            self.screen = Screen::Running;
            self.play_dialog = false;
        }
        self.servers.poll(ctx);

        // Animate the dialog open/close alpha.
        let dialog_target = if self.play_dialog { 1.0 } else { 0.0 };
        self.dialog_alpha = (self.dialog_alpha + (dialog_target - self.dialog_alpha) * 0.18)
            .clamp(0.0, 1.0);
        if self.dialog_alpha > 0.0 && self.dialog_alpha < 1.0 {
            ctx.request_repaint();
        }

        self.dialog_backdrop(ctx);

        // Left rail navigation, CurseForge-style: brand on top, screens as
        // full-width rows with a lighter active shade, index actions pinned
        // at the bottom.
        egui::SidePanel::left("nav")
            .exact_width(208.0)
            .resizable(false)
            .frame(Frame::new().fill(PANEL).inner_margin(egui::Margin::same(16)))
            .show(ctx, |ui| {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new("CLOUD 9")
                            .monospace()
                            .strong()
                            .color(ACCENT)
                            .size(20.0),
                    );
                    ui.add_space(16.0);
                    for (screen, label) in [
                        (Screen::Episodes, "Episodes"),
                        (Screen::Instances, "Instances"),
                        (Screen::Running, "Running"),
                        (Screen::Servers, "Servers"),
                    ] {
                        if nav_item(ui, self.screen == screen, label) {
                            self.screen = screen;
                        }
                        ui.add_space(4.0);
                    }
                    ui.add_space(12.0);
                    ui.separator();
                    ui.add_space(12.0);
                    if ui
                        .add_sized(
                            [ui.available_width(), 32.0],
                            egui::Button::new(RichText::new("Reload index").small()),
                    )
                    .clicked()
                    {
                        self.open();
                    }
                    ui.add_space(8.0);
                    match &self.error {
                        Some(error) => {
                            ui.colored_label(ERROR, RichText::new(error).small());
                        }
                        None => {
                            let detail = match self.catalog_version {
                                Some(version) => format!("Index v{version}"),
                                None => "No index".to_owned(),
                            };
                            ui.label(RichText::new(detail).small().color(MUTED));
                        }
                    }
                });
            });

        egui::CentralPanel::default()
            .frame(Frame::new().fill(GROUND).inner_margin(28))
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
                                let matchup = cloud9_server::Matchup::load(&path)
                                    .map_err(|error| error.to_string())?;
                                let document = self
                                    .catalog
                                    .as_ref()
                                    .ok_or_else(|| {
                                        "Open an episode catalog before importing a matchup"
                                            .to_owned()
                                    })?
                                    .document(&matchup.manifest_id)
                                    .map_err(|_| {
                                        format!(
                                            "Add episode {} to the catalog before importing this matchup",
                                            matchup.manifest_id
                                        )
                                    })?;
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
                    ui.heading(RichText::new("Episodes").size(30.0));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{} shown", self.cards.len()))
                                .small()
                                .color(MUTED),
                        );
                    });
                });
                ui.add_space(14.0);
                let mut changed = ui
                    .add_sized(
                        [ui.available_width(), 38.0],
                        egui::TextEdit::singleline(&mut self.filters.search)
                            .hint_text("Search episode titles or IDs"),
                    )
                    .changed();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing = vec2(10.0_f32, 0.0_f32);
                    let previous = self.filters.mode.clone();
                    egui::ComboBox::from_id_salt("mode")
                        .selected_text(RichText::new(mode_label(&self.filters.mode)).small())
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
                                .desired_width(170.0)
                                .hint_text("Addon or mob"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::TextEdit::singleline(&mut self.filters.version)
                                .desired_width(115.0)
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
                ui.add_space(10.0);
                if let Some(error) = &self.error {
                    ui.colored_label(ERROR, error);
                    ui.label("Create the SQLite index with the scraper, then reload.");
                    return;
                }
                if self.cards.is_empty() {
                    ui.colored_label(MUTED, "No episodes match these filters.");
                    return;
                }

                let gap = 16.0;
                let width = (ui.available_width() - 64.0).max(200.0);
                let columns = gallery_columns(width, gap);
                let card_width =
                    ((width - gap * (columns - 1) as f32) / columns as f32).min(480.0);
                let row_height = card_width * 9.0 / 16.0 + 190.0;
                let mut selected = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, row_height, self.cards.len().div_ceil(columns), |ui, rows| {
                        for row in rows {
                            let start = row * columns;
                            ui.horizontal_top(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                ui.set_min_height(row_height);
                                for card in self.cards[start..(start + columns).min(self.cards.len())].iter()
                                {
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
                        ui.heading(RichText::new("Episode setup").size(20.0));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.small_button("Close").clicked()
                        })
                        .inner
                    })
                    .inner;
                egui::ScrollArea::vertical()
                    .max_height((ctx.screen_rect().height() - 140.0).max(110.0))
                    .show(ui, |ui| self.runner.plan(ui));
                close
            });
            if dialog.inner || dialog.should_close() {
                self.play_dialog = false;
                self.dialog_alpha = 0.0;
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

/// Full-width sidebar row with a lighter active shade, CurseForge-style.
/// Returns true when clicked.
fn nav_item(ui: &mut egui::Ui, selected: bool, label: &str) -> bool {
    let (rect, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 36.0), egui::Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 4.0, ACCENT_FAINT);
    } else if response.hovered() {
        ui.painter()
            .rect_filled(rect, 4.0, Color32::from_white_alpha(8));
    }
    ui.painter().text(
        pos2(rect.min.x + 14.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        FontId::proportional(15.0),
        if selected { TEXT } else { MUTED },
    );
    response.clicked()
}

fn gallery_columns(width: f32, gap: f32) -> usize {
    (((width + gap) / (260.0 + gap)).floor() as usize).clamp(1, 2)
}

fn render_card(
    ui: &mut egui::Ui,
    card: &EpisodeCard,
    thumbnails: &mut Thumbnails,
    width: f32,
) -> bool {
    Frame::new()
        .fill(SURFACE)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .corner_radius(CARD_RADIUS)
        .show(ui, |ui| {
            ui.vertical(|ui| {
                ui.set_width(width);
                ui.spacing_mut().item_spacing.y = 8.0;

                // Thumbnail band — full width of card.
            let opened = thumbnails.draw(ui, card, width).clicked();

            ui.add_space(8.0);

            // Content — stacked vertically below the thumbnail.
            ui.vertical(|ui| {
                ui.set_width(width - 24.0);
                ui.spacing_mut().item_spacing.y = 6.0;

                // Mode chip + version, aligned to the right.
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(mode_label(&card.mode))
                            .color(ACCENT)
                            .small()
                            .strong(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(&card.version)
                                .monospace()
                                .color(TEXT_DIM)
                                .small(),
                        );
                    });
                });

                ui.add_space(4.0);

                // Title, up to two lines.
                let mut title = egui::text::LayoutJob::simple(
                    card.title.clone(),
                    FontId::proportional(17.0),
                    TEXT,
                    width - 24.0,
                );
                title.wrap.max_rows = 2;
                ui.allocate_ui_with_layout(
                    vec2(width - 24.0_f32, 44.0_f32),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.label(title).on_hover_text(&card.title);
                    },
                );

                ui.add_space(2.0);

                // Status / tags line.
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

                ui.add_space(10.0);

                // Action row: primary action left, watch link right.
                let action_clicked = ui
                    .horizontal(|ui| {
                        let b = ui
                            .small_button(if card.community_setup {
                                "Play"
                            } else {
                                "Details"
                            })
                            .clicked();
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.hyperlink_to("Watch episode", &card.url);
                            },
                        );
                        b
                    })
                    .inner;
                opened || action_clicked
                })
                .inner
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
        assert_eq!(gallery_columns(255.0, 16.0), 1);
        assert_eq!(gallery_columns(736.0, 16.0), 2);
        assert_eq!(gallery_columns(1000.0, 16.0), 2);
        assert_eq!(gallery_columns(1600.0, 16.0), 2);
    }

    #[test]
    fn thumbnail_crops_without_stretching() {
        let uv = thumbnail_uv(4.0 / 3.0, 16.0 / 9.0);
        assert_eq!(uv.width(), 1.0);
        assert!((uv.height() - 0.75).abs() < 0.01);
    }
}
