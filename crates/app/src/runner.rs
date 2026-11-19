use crate::{ACCENT, ERROR, GROUND, MUTED, OK, PANEL, TEXT};
use cloud9_core::Document;
use cloud9_mc::{
    Account, ClientRuntime, Installer, Instance, Level, LogEvent, MicrosoftSignIn, Profile,
    bootstrap_client_registry, create_instance_with_helper,
};
use eframe::egui::{self, Frame, RichText};
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
    Error(String),
    Notice(String),
    Launched(Child, PathBuf, Receiver<LogEvent>),
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
    ram: u32,
    pending: bool,
    verification: Option<(String, String)>,
    cancel: Arc<AtomicBool>,
    send: Sender<Update>,
    update: Receiver<Update>,
    status: String,
    error: bool,
    process: Option<Child>,
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
            client_id: std::env::var("CLOUD9_MICROSOFT_CLIENT_ID").unwrap_or_default(),
            dev: false,
            account: None,
            helper: "helper-mod/build/libs/runner-helper-0.1.0.jar".into(),
            template_root: "templates".into(),
            previous: None,
            ram: 2048,
            pending: false,
            verification: None,
            cancel: Arc::new(AtomicBool::new(false)),
            send,
            update,
            status: String::new(),
            error: false,
            process: None,
            log_stream: None,
            logfile: None,
            events: VecDeque::new(),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        for update in self.update.try_iter() {
            match update {
                Update::Verification(url, code) => self.verification = Some((url, code)),
                Update::SignedIn(account) => {
                    self.account = Some(Arc::new(account));
                    self.verification = None;
                    self.pending = false;
                    self.status = "Microsoft account connected".into();
                }
                Update::Error(error) => {
                    self.error = true;
                    self.status = error;
                    self.verification = None;
                    self.pending = false;
                }
                Update::Notice(notice) => self.status = notice,
                Update::Launched(child, path, stream) => {
                    self.status = format!("Minecraft running · {}", path.display());
                    self.pending = false;
                    self.events.clear();
                    self.logfile = OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(path.join("logs/runner.log"))
                        .ok();
                    self.previous = Some(path);
                    self.process = Some(child);
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
    }

    pub fn select(&mut self, result: Result<Document, String>) {
        self.previous = None;
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
        let supplied = PathBuf::from(&self.supplied);
        let java = (!self.java.is_empty()).then(|| PathBuf::from(&self.java));
        let ram = self.ram;
        let helper = PathBuf::from(&self.helper);
        let templates = PathBuf::from(&self.template_root);
        let previous = reset.then(|| self.previous.clone()).flatten();
        std::thread::spawn(move || {
            let result = (|| -> Result<_, Box<dyn std::error::Error>> {
                let profile = Profile::primary()?;
                let cache = PathBuf::from(".cache/minecraft");
                let instance = if let Some(previous) = previous {
                    Instance::open(&previous)?
                } else {
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
                let _ = sender.send(Update::Notice(
                    "Verifying and installing the Minecraft runtime".into(),
                ));
                let runtime = ClientRuntime::prepare(&Installer::new(&cache)?, &instance)?;
                let _ = sender.send(Update::Notice("Checking the item registry".into()));
                let registry =
                    bootstrap_client_registry(&runtime, &instance, &cache.join("registries"), ram)?;
                let round = cloud9_world::EpisodeWorld::open(
                    &document,
                    instance.root.join("saves/Cloud9"),
                    instance.root.join("round-snapshot"),
                    &templates,
                    &PathBuf::from("aliases"),
                    &instance.mods,
                    &registry,
                    &instance.mod_set_hash,
                    "cloud9-round",
                )?;
                if reset {
                    round.reset()?;
                } else {
                    round.prepare(&templates.join(&document.manifest().map.template_id))?;
                }
                let (logs, receive) = channel();
                let child = runtime.launch(&instance, &account, ram, None, logs)?;
                Ok(Update::Launched(child, instance.root, receive))
            })();
            let _ = sender.send(result.unwrap_or_else(|error| Update::Error(error.to_string())));
        });
    }

    pub fn plan(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("Run plan").size(31.0));
        ui.add_space(8.0);
        if let Some(document) = &self.selected {
            let episode = document.manifest();
            ui.label(RichText::new(&episode.episode.title).size(20.0));
            ui.colored_label(
                MUTED,
                format!(
                    "Minecraft {} · {} · Java {}",
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
            for jar in &episode.mods {
                ui.label(&jar.display_name.value);
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Local mod directory");
                ui.text_edit_singleline(&mut self.supplied);
            });
            ui.horizontal(|ui| {
                ui.label("Java executable");
                ui.add(egui::TextEdit::singleline(&mut self.java).hint_text("Auto-detect"));
            });
            ui.horizontal(|ui| {
                ui.label("Runner helper JAR");
                ui.text_edit_singleline(&mut self.helper);
            });
            ui.horizontal(|ui| {
                ui.label("Templates");
                ui.text_edit_singleline(&mut self.template_root);
            });
            ui.add(
                egui::Slider::new(&mut self.ram, 1024..=16384)
                    .step_by(256.0)
                    .suffix(" MiB")
                    .text("RAM"),
            );
            ui.separator();
            ui.checkbox(&mut self.dev, "Local development profile");
            if self.dev {
                ui.colored_label(ACCENT, "Offline development session for local testing.");
            } else if let Some(account) = &self.account {
                ui.colored_label(OK, format!("Signed in as {}", account.name));
            } else {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.client_id)
                            .hint_text("Microsoft application client ID"),
                    );
                    if ui
                        .add_enabled(!self.pending, egui::Button::new("Sign in"))
                        .clicked()
                    {
                        self.sign_in();
                    }
                });
            }
            if let Some((url, code)) = &self.verification {
                ui.horizontal(|ui| {
                    ui.hyperlink_to("Open Microsoft sign-in", url);
                    if ui.button(format!("Copy code {code}")).clicked() {
                        ui.ctx().copy_text(code.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                    }
                });
            }
            if ui
                .add_enabled(
                    !self.pending && self.process.is_none() && (self.dev || self.account.is_some()),
                    egui::Button::new("Launch client").fill(PANEL),
                )
                .clicked()
            {
                self.launch(false);
            }
        } else {
            ui.colored_label(MUTED, "Choose an episode in the browser.");
        }
        self.status(ui);
    }

    pub fn running(&mut self, ui: &mut egui::Ui) {
        ui.heading(RichText::new("Running").size(31.0));
        self.status(ui);
        if self.previous.is_some()
            && self.selected.is_some()
            && ui
                .add_enabled(
                    !self.pending && self.process.is_none() && (self.dev || self.account.is_some()),
                    egui::Button::new("Reset round and launch"),
                )
                .clicked()
        {
            self.launch(true);
        }
        if self.events.is_empty() {
            ui.colored_label(MUTED, "Minecraft output will appear here.");
        }
        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .show(ui, |ui| {
                Frame::new().fill(GROUND).inner_margin(6).show(ui, |ui| {
                    for event in &self.events {
                        let (label, color) = match event.level {
                            Level::Info => ("Info", TEXT),
                            Level::Warn => ("Warning", ACCENT),
                            Level::Error => ("Error", ERROR),
                        };
                        ui.label(
                            RichText::new(format!("[{label}] {}", event.line))
                                .monospace()
                                .color(color),
                        );
                        if let Some(diagnostic) = event.diagnostic {
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

    fn status(&self, ui: &mut egui::Ui) {
        if !self.status.is_empty() {
            ui.colored_label(if self.error { ERROR } else { MUTED }, &self.status);
        }
    }
}
