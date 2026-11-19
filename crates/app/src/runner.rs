use crate::{
    ACCENT, BORDER, ERROR, GROUND, MUTED, OK, PANEL, SURFACE, TEXT, TEXT_DIM,
};
use cloud9_core::Document;
use cloud9_mc::{
    Account, ClientRuntime, Installer, Instance, Level, LogEvent, MicrosoftSignIn, Profile,
    bootstrap_client_registry, create_instance, create_instance_with_helper, download_pinned_mods,
    has_mod_downloads, resolve_mods,
};
use eframe::egui::{
    self, CollapsingHeader, Frame, RichText, Stroke, vec2,
};
use std::{
    collections::VecDeque,
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    process::Child,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender, channel},
    },
    time::Duration,
};

enum Update {
    Verification(String, String),
    SignedIn(Account),
    ModsReady(usize),
    Error(String),
    Notice(String),
    Launched(Child, PathBuf, Receiver<LogEvent>, std::fs::File),
}

pub struct Runner {
    pub selected: Option<Document>,
    supplied: String,
    java: String,
    client_id: String,
    dev: bool,
    account: Option<Arc<Account>>,
    helper: String,
    template_root: String,
    previous: Option<PathBuf>,
    latest: Option<PathBuf>,
    action_focus: Option<&'static str>,
    ignored_message: Option<&'static str>,
    matchup: Option<cloud9_server::Matchup>,
    ram: u32,
    pending: bool,
    verification: Option<(String, String)>,
    cancel: Arc<AtomicBool>,
    send: Sender<Update>,
    update: Receiver<Update>,
    status: String,
    error: bool,
    process: Option<Child>,
    session_lock: Option<std::fs::File>,
    saved: Option<PathBuf>,
    saved_lock: Option<Instance>,
    log_stream: Option<Receiver<LogEvent>>,
    logfile: Option<std::fs::File>,
    events: VecDeque<LogEvent>,
}

impl Runner {
    pub fn new() -> Self {
        let (send, update) = channel();
        Self {
            selected: None,
            supplied: "local-mods".into(),
            java: String::new(),
            client_id: std::env::var("CLOUD9_MICROSOFT_CLIENT_ID")
                .ok()
                .or_else(|| option_env!("CLOUD9_MICROSOFT_CLIENT_ID").map(str::to_owned))
                .unwrap_or_default(),
            dev: false,
            account: None,
            helper: "helper-mod/build/libs/runner-helper-0.1.0.jar".into(),
            template_root: "templates".into(),
            previous: None,
            latest: None,
            action_focus: None,
            ignored_message: None,
            matchup: None,
            ram: 2048,
            pending: false,
            verification: None,
            cancel: Arc::new(AtomicBool::new(false)),
            send,
            update,
            status: String::new(),
            error: false,
            process: None,
            session_lock: None,
            saved: None,
            saved_lock: None,
            log_stream: None,
            logfile: None,
            events: VecDeque::new(),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut launched = false;
        for update in self.update.try_iter() {
            match update {
                Update::Verification(url, code) => self.verification = Some((url, code)),
                Update::SignedIn(account) => {
                    self.account = Some(Arc::new(account));
                    self.verification = None;
                    self.pending = false;
                    self.status = "Microsoft account connected".into();
                }
                Update::ModsReady(count) => {
                    self.pending = false;
                    self.status = format!("{count} pinned mod files verified");
                }
                Update::Error(error) => {
                    self.error = true;
                    self.status = error;
                    self.verification = None;
                    self.pending = false;
                }
                Update::Notice(notice) => self.status = notice,
                Update::Launched(child, path, stream, lock) => {
                    self.status = format!("Minecraft running · {}", path.display());
                    self.pending = false;
                    self.events.clear();
                    self.ignored_message = None;
                    self.logfile = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path.join("logs/runner.log"))
                        .ok();
                    launched = true;
                    self.latest = Some(path.clone());
                    self.previous = path.join("round-snapshot").is_dir().then_some(path);
                    self.process = Some(child);
                    self.session_lock = Some(lock);
                    self.log_stream = Some(stream);
                }
            }
        }
        if let Some(stream) = &self.log_stream {
            for event in stream.try_iter().take(300) {
                if let Some(file) = &mut self.logfile {
                    let _ = writeln!(file, "{}", event.line);
                }
                if self.events.len() == 1000 {
                    self.events.pop_front();
                }
                self.events.push_back(event);
            }
        }
        if let Some(process) = &mut self.process {
            match process.try_wait() {
                Ok(Some(status)) => {
                    self.error = !status.success();
                    self.status = format!("Minecraft exited ({status})");
                    self.process = None;
                    self.session_lock = None;
                }
                Err(error) => {
                    self.status = error.to_string();
                    self.error = true;
                }
                Ok(None) => {}
            }
        }
        if self.pending || self.process.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        launched
    }

    pub fn select(&mut self, result: Result<Document, String>) {
        self.previous = None;
        self.matchup = None;
        self.saved = None;
        self.saved_lock = None;
        match result {
            Ok(document) => {
                self.selected = Some(document);
                if !self.pending && self.process.is_none() {
                    self.status.clear();
                    self.error = false;
                }
            }
            Err(error) => {
                self.selected = None;
                self.status = error;
                self.error = true;
            }
        }
    }

    pub fn select_saved(&mut self, path: PathBuf) {
        let multiplayer =
            path.join("servers.dat").is_file() && !path.join("round-snapshot").is_dir();
        match Document::load(&path.join("episode.json")) {
            Ok(document) => {
                self.select(Ok(document));
                self.saved_lock = Instance::inspect(&path).ok();
                self.saved = Some(path);
                if multiplayer {
                    self.dev = false;
                }
            }
            Err(error) => self.select(Err(error.to_string())),
        }
    }

    pub fn import(&mut self, input: Result<(Document, cloud9_server::Matchup), String>) {
        match input {
            Ok((document, matchup)) => {
                self.select(Ok(document));
                self.matchup = Some(matchup);
                self.dev = false;
            }
            Err(error) => self.select(Err(error)),
        }
    }

    pub fn paths(&self) -> (PathBuf, Option<PathBuf>, PathBuf, PathBuf) {
        (
            PathBuf::from(&self.supplied),
            (!self.java.is_empty()).then(|| PathBuf::from(&self.java)),
            PathBuf::from(&self.helper),
            PathBuf::from(&self.template_root),
        )
    }

    fn sign_in(&mut self) {
        self.pending = true;
        self.error = false;
        self.status = "Starting Microsoft sign-in".into();
        self.cancel.store(false, Ordering::Relaxed);
        let cancel = Arc::clone(&self.cancel);
        let sender = self.send.clone();
        let id = self.client_id.clone();
        std::thread::spawn(move || {
            let result = (|| {
                let signin = MicrosoftSignIn::begin(&id)?;
                let (url, code) = signin.verification();
                let _ = sender.send(Update::Verification(url.to_owned(), code.to_owned()));
                signin.finish(|| cancel.load(Ordering::Relaxed))
            })();
            let _ = sender.send(match result {
                Ok(account) => Update::SignedIn(account),
                Err(error) => Update::Error(error.to_string()),
            });
        });
    }

    fn launch(&mut self, reset: bool) {
        let Some(document) = self.selected.clone() else {
            return;
        };
        let account = if self.dev {
            Account::local_dev("Cloud9Dev").ok().map(Arc::new)
        } else {
            self.account.clone()
        };
        let Some(account) = account else {
            return;
        };
        self.pending = true;
        self.error = false;
        self.status = "Checking Java and mod jars".into();
        let sender = self.send.clone();
        let (supplied, java, helper, templates) = self.paths();
        let ram = self.ram;
        let matchup = if reset { None } else { self.matchup.clone() };
        let saved = if reset {
            self.saved.clone().or_else(|| self.previous.clone())
        } else {
            self.saved.clone()
        };
        std::thread::spawn(move || {
            let result = (|| -> Result<_, Box<dyn std::error::Error>> {
                let mut document = document;
                let cache = PathBuf::from(".cache/minecraft");
                if saved.is_none() && has_mod_downloads(document.manifest()) {
                    let mods = cache.join("mods");
                    if matches!(resolve_mods(document.manifest(), &supplied, &mods, false),
                        Err(cloud9_mc::Error::Config(message)) if message.starts_with("Locate jar"))
                    {
                        let _ = sender.send(Update::Notice(
                            "Downloading and verifying pinned mods…".into(),
                        ));
                        download_pinned_mods(document.manifest(), &mods)?;
                    }
                }
                if let Some(matchup) = &matchup {
                    let profile = Profile::selected()?;
                    let instance = create_instance(
                        &PathBuf::from("instances"),
                        &cache,
                        &document,
                        &profile,
                        &supplied,
                        java.as_deref(),
                    )?;
                    let lock = instance.session_lock()?;
                    cloud9_world::bookmark_server(
                        &instance.root,
                        &document.manifest().episode.title,
                        &matchup.address,
                    )?;
                    let runtime = ClientRuntime::prepare(&Installer::new(&cache)?, &instance)?;
                    let (logs, receive) = channel();
                    let child = runtime.launch(
                        &instance,
                        &account,
                        ram,
                        Some(matchup.host_port()?),
                        logs,
                    )?;
                    return Ok(Update::Launched(child, instance.root, receive, lock));
                }
                let existing = saved.is_some();
                let instance = if let Some(path) = saved {
                    document = Document::load(&path.join("episode.json"))?;
                    Instance::open(&path)?
                } else {
                    let profile = Profile::selected()?;
                    create_instance_with_helper(
                        &PathBuf::from("instances"),
                        &cache,
                        &document,
                        &profile,
                        &supplied,
                        java.as_deref(),
                        &helper,
                    )?
                };
                let lock = instance.session_lock()?;
                let has_snapshot = instance.root.join("round-snapshot").is_dir();
                let multiplayer =
                    existing && !has_snapshot && instance.root.join("servers.dat").is_file();
                if multiplayer && !account.authenticated() {
                    return Err(
                        "Microsoft sign-in required for a saved multiplayer instance".into(),
                    );
                }
                let _ = sender.send(Update::Notice(
                    "Verifying and installing the Minecraft runtime".into(),
                ));
                let runtime = ClientRuntime::prepare(&Installer::new(&cache)?, &instance)?;
                if reset || !has_snapshot && !multiplayer {
                    let _ = sender.send(Update::Notice("Checking the item registry".into()));
                    let registry = bootstrap_client_registry(
                        &runtime,
                        &instance,
                        &cache.join("registries"),
                        ram,
                        &account,
                    )?;
                    let round = cloud9_world::EpisodeWorld::open(
                        &document,
                        instance.root.join("saves/Cloud9"),
                        instance.root.join("round-snapshot"),
                        cloud9_world::SetupContext {
                            templates: &templates,
                            aliases: &PathBuf::from("aliases"),
                            mods: &instance.mods,
                            registry: &registry,
                            mod_set_hash: &instance.mod_set_hash,
                            run_id: "cloud9-round",
                        },
                    )?;
                    if reset {
                        round.reset()?;
                    } else {
                        round.prepare()?;
                    }
                }
                let (logs, receive) = channel();
                let child = if multiplayer {
                    runtime.launch(&instance, &account, ram, None, logs)?
                } else {
                    runtime.launch_prepared(&instance, &account, ram, logs)?
                };
                Ok(Update::Launched(child, instance.root, receive, lock))
            })();
            let _ = sender.send(result.unwrap_or_else(|error| Update::Error(error.to_string())));
        });
    }

    fn download_mods(&mut self) {
        let Some(document) = self.selected.clone() else {
            return;
        };
        self.pending = true;
        self.error = false;
        self.status = "Downloading and verifying pinned mods…".into();
        let sender = self.send.clone();
        std::thread::spawn(move || {
            let result = download_pinned_mods(
                document.manifest(),
                std::path::Path::new(".cache/minecraft/mods"),
            );
            let _ = sender.send(match result {
                Ok(count) => Update::ModsReady(count),
                Err(error) => Update::Error(error.to_string()),
            });
        });
    }

    /// A form row: label on the left, editable value on the right.
    fn form_row(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        value: &mut String,
        hint: Option<&str>,
        focus: bool,
    ) -> egui::Response {
        ui.horizontal(|ui| {
            ui.label(RichText::new(label).color(TEXT_DIM).small());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let mut edit = egui::TextEdit::singleline(value);
                if let Some(h) = hint {
                    edit = edit.hint_text(h);
                }
                let response = ui.add(edit);
                if focus {
                    response.request_focus();
                }
                response
            })
            .inner
        })
        .inner
    }

    pub fn plan(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        if let Some(document) = &self.selected {
            let episode = document.manifest();
            ui.heading(RichText::new(&episode.episode.title).size(22.0));
            ui.add_space(2.0);
            ui.colored_label(
                MUTED,
                format!(
                    "Minecraft {} · Forge {} · Java {}",
                    episode.game.mc_version.value,
                    episode
                        .game
                        .loader
                        .version
                        .value
                        .as_deref()
                        .unwrap_or("version pending review"),
                    episode
                        .game
                        .java_major
                        .value
                        .map_or_else(|| "pending review".to_owned(), |major| major.to_string())
                ),
            );
            ui.add_space(10.0);

            // Map info.
            ui.horizontal(|ui| {
                ui.label(RichText::new("Map").color(TEXT_DIM).small());
                let map = match episode.map.template_id.as_str() {
                    "challenge_games" => "Cloud 9 Arena",
                    "lucky_block_bedwars" => "Cloud 9 Islands",
                    name => name,
                };
                ui.label(RichText::new(map).strong());
            });
            ui.add_space(8.0);

            // Mods list.
            for jar in &episode.mods {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&jar.display_name.value).small());
                    if let Some(source) = jar.file_hint.as_ref().and_then(|hint| hint.url.as_ref()) {
                        ui.hyperlink_to("Source", source);
                    }
                });
            }
            if episode.mods.is_empty() {
                ui.colored_label(MUTED, "No pinned mods.");
            }

            if episode.source()["map"]["overrides"]["challenge"]["kind"] == "chicken_jockey" {
                ui.add_space(6.0);
                ui.colored_label(ACCENT, "Community round");
                ui.label("Open Lucky Blocks, trade with the Arena Trader, then right-click the gold block to fight.");
            }

            ui.add_space(10.0);

            // Download pinned mods.
            if self.saved.is_none()
                && has_mod_downloads(episode)
                && ui
                    .add_enabled(!self.pending, egui::Button::new("Download pinned mods"))
                    .clicked()
            {
                self.download_mods();
            }

            ui.separator();
            ui.add_space(4.0);

            // Server / sign-in status.
            let saved_server = self.saved.as_ref().is_some_and(|path| {
                path.join("servers.dat").is_file() && !path.join("round-snapshot").is_dir()
            });
            if let Some(matchup) = &self.matchup {
                ui.colored_label(ACCENT, format!("Join {}", matchup.address));
            } else if saved_server {
                ui.colored_label(
                    ACCENT,
                    "Open Multiplayer to connect to the bookmarked server.",
                );
            }

            let can_use_dev = self.matchup.is_none() && !saved_server;
            if self.dev {
                ui.colored_label(ACCENT, "Offline development session for local testing.");
            } else if let Some(account) = &self.account {
                let name = account.name.clone();
                ui.horizontal(|ui| {
                    ui.colored_label(OK, format!("Signed in as {name}"));
                    if ui.button("Sign out").clicked() {
                        self.account = None;
                    }
                });
            } else if self.client_id.trim().is_empty() {
                ui.colored_label(MUTED, "Microsoft sign-in needs an approved Cloud 9 build.");
            } else if ui
                .add_enabled(!self.pending, egui::Button::new("Sign in with Microsoft"))
                .clicked()
            {
                self.sign_in();
            }
            if let Some((url, code)) = &self.verification {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.hyperlink_to("Open Microsoft sign-in", url);
                    ui.add_space(8.0);
                    if ui.button(format!("Copy code {code}")).clicked() {
                        ui.ctx().copy_text(code.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                });
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(4.0);

            // Advanced settings.
            let focus_advanced = matches!(
                self.action_focus,
                Some("Locate jar" | "Fetch" | "Locate Java" | "Increase RAM")
            );
            CollapsingHeader::new("Advanced settings")
                .open(focus_advanced.then_some(true))
                .show(ui, |ui| {
                    if can_use_dev {
                        ui.add_space(2.0);
                        ui.checkbox(&mut self.dev, "Local development profile");
                        ui.add_space(4.0);
                    }
                    if let Some(path) = &self.saved {
                        ui.label(RichText::new("Instance folder").color(TEXT_DIM).small());
                        ui.label(format!("{}", path.display()));
                        if let Some(instance) = &self.saved_lock {
                            ui.colored_label(
                                MUTED,
                                format!("Locked Java executable {}", instance.java.display()),
                            );
                            for locked in &instance.mods {
                                ui.monospace(format!("{} {}", locked.mod_id, locked.version));
                            }
                        }
                        ui.add_space(4.0);
                    }
                    ui.add_enabled_ui(self.saved.is_none(), |ui| {
                        self.form_row(ui, "Local mod directory", &mut self.supplied, None,
                            matches!(self.action_focus, Some("Locate jar" | "Fetch")));
                        ui.add_space(4.0);
                        self.form_row(ui, "Java executable", &mut self.java, Some("Auto-detect"),
                            self.action_focus == Some("Locate Java"));
                        ui.add_space(4.0);
                        self.form_row(ui, "Runner helper JAR", &mut self.helper, None, false);
                        ui.add_space(4.0);
                        self.form_row(ui, "Templates", &mut self.template_root, None, false);
                    });
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("RAM").color(TEXT_DIM).small());
                        let slider = egui::Slider::new(&mut self.ram, 1024..=16384)
                            .step_by(256.0)
                            .suffix(" MiB");
                        let response = ui.add(slider);
                        if self.action_focus == Some("Increase RAM") {
                            response.request_focus();
                            self.action_focus = None;
                        }
                    });
                });

            ui.add_space(10.0);

            // Primary action buttons.
            let can_launch = !self.pending && self.process.is_none() && (self.dev || self.account.is_some());
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        can_launch,
                        egui::Button::new(
                            RichText::new(if self.matchup.is_some() {
                                "Join server"
                            } else if self.saved.is_some() {
                                "Resume"
                            } else {
                                "Play"
                            })
                            .strong()
                            .color(GROUND),
                    )
                    .fill(ACCENT)
                    .clicked()
                {
                    self.launch(false);
                }
                if self
                    .saved
                    .as_ref()
                    .is_some_and(|path| path.join("round-snapshot").is_dir())
                    && ui
                        .add_enabled(
                            can_launch,
                            egui::Button::new("Start fresh round"),
                        )
                        .clicked()
                {
                    self.launch(true);
                }
            });

            // Error recovery actions.
            if self.error && self.status.starts_with("Locate jar")
                && ui.button("Locate jar").clicked()
            {
                self.action_focus = Some("Locate jar");
            }
            if self.error && self.status.starts_with("Edit alias") {
                ui.label("Edit aliases/<mod_id>/<version>.json, then retry.");
            }
            self.status(ui);
        } else {
            ui.colored_label(MUTED, "Choose an episode in the browser.");
        }
    }

    pub fn running(&mut self, ui: &mut egui::Ui) -> bool {
        ui.heading(RichText::new("Running").size(32.0));
        ui.add_space(6.0);
        self.status(ui);
        ui.add_space(8.0);

        if let Some(path) = self.previous.clone()
            && ui
                .add_enabled(
                    !self.pending && self.process.is_none() && (self.dev || self.account.is_some()),
                    egui::Button::new("Reset round and launch"),
                )
                .clicked()
        {
            self.select_saved(path);
            self.launch(true);
        }
        if let Some(path) = &self.latest
            && ui.button("Bundle local logs").clicked()
        {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("run");
            let destination = PathBuf::from(".cache/reports").join(format!("{name}.zip"));
            match cloud9_mc::support_bundle(path, &destination) {
                Ok(_) => {
                    self.error = false;
                    self.status = format!("Log bundle saved to {}", destination.display());
                }
                Err(error) => {
                    self.error = true;
                    self.status = error.to_string();
                }
            }
        }

        ui.add_space(10.0);
        if self.events.is_empty() {
            ui.colored_label(MUTED, "Minecraft output will appear here.");
        } else {
            let diagnostic = self
                .events
                .iter()
                .rev()
                .find_map(|event| event.diagnostic)
                .filter(|diagnostic| Some(diagnostic.message) != self.ignored_message);
            if let Some(diagnostic) = diagnostic {
                ui.colored_label(ACCENT, diagnostic.message);
                ui.horizontal_wrapped(|ui| {
                    for action in diagnostic.actions {
                        if ui.button(*action).clicked() {
                            if *action == "Ignore" {
                                self.ignored_message = Some(diagnostic.message);
                            } else {
                                self.action_focus = Some(action);
                                if *action == "Edit alias" {
                                    self.status = "Edit alias: update aliases/<mod_id>/<version>.json, then retry.".into();
                                    self.error = true;
                                }
                                if *action == "Fetch" {
                                    self.status = "Use the mod source links and place the matching jars in the local mod directory.".into();
                                    self.error = false;
                                }
                            }
                        }
                    }
                });
                ui.add_space(6.0);
            }

            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    Frame::new()
                        .fill(GROUND)
                        .stroke(Stroke::new(1.0_f32, BORDER))
                        .corner_radius(crate::CARD_RADIUS)
                        .inner_margin(egui::Margin::same(10.0))
                        .show(ui, |ui| {
                            for event in &self.events {
                                let (label, color) = match event.level {
                                    Level::Info => ("Info", TEXT),
                                    Level::Warn => ("Warning", ACCENT),
                                    Level::Error => ("Error", ERROR),
                                };
                                ui.label(
                                    RichText::new(format!("[{label}] {}", event.line))
                                        .monospace()
                                        .size(12.0)
                                        .color(color),
                                );
                                if let Some(diagnostic) = &event.diagnostic {
                                    ui.colored_label(
                                        color,
                                        format!(
                                            "{} · {}",
                                            diagnostic.message,
                                            diagnostic.actions.join(" / ")
                                        ),
                                    );
                                }
                            }
                        });
                });
        }

        false
    }

    fn status(&self, ui: &mut egui::Ui) {
        if !self.status.is_empty() {
            ui.colored_label(if self.error { ERROR } else { MUTED }, &self.status);
        }
    }
}
