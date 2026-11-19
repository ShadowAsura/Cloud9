use crate::{
    ACCENT, BORDER, ERROR, GROUND, MUTED, OK, PANEL, SURFACE, TEXT, TEXT_DIM, runner::Runner,
};
use cloud9_server::{Control, Create, EULA_URL, Matchup, Options, Server, Status};
use eframe::egui::{self, RichText, Stroke, vec2};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, channel},
    },
    time::{Duration, Instant},
};

enum ResultEvent {
    Opened(Box<Server>),
    Finished,
    Error(String),
}

pub struct Servers {
    slug: String,
    share: String,
    accept: bool,
    options: Options,
    current: Option<Box<Server>>,
    status: Option<Status>,
    pending: Option<Receiver<ResultEvent>>,
    worker: Option<std::thread::JoinHandle<()>>,
    supervises: bool,
    network: Option<Receiver<String>>,
    cancellation: Arc<AtomicBool>,
    next_refresh: Instant,
    message: String,
    error: bool,
    console: String,
    logs: String,
}

impl Servers {
    pub fn new() -> Self {
        Self {
            slug: "match".into(),
            share: "cloud9.matchup".into(),
            accept: false,
            options: Options::default(),
            current: None,
            status: None,
            pending: None,
            worker: None,
            supervises: false,
            network: None,
            cancellation: Arc::new(AtomicBool::new(false)),
            next_refresh: Instant::now(),
            message: String::new(),
            error: false,
            console: String::new(),
            logs: String::new(),
        }
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if let Some(event) = self
            .pending
            .as_ref()
            .and_then(|channel| channel.try_recv().ok())
        {
            self.pending = None;
            self.worker.take();
            self.supervises = false;
            match event {
                ResultEvent::Opened(server) => {
                    self.message = format!("Server ready at {}", server.address);
                    self.current = Some(server);
                    self.status = None;
                    self.logs.clear();
                    self.accept = false;
                }
                ResultEvent::Finished => self.message = "Server stopped".into(),
                ResultEvent::Error(message) => {
                    self.message = message;
                    self.error = true;
                }
            }
        }
        if let Some(report) = self
            .network
            .as_ref()
            .and_then(|channel| channel.try_recv().ok())
        {
            self.message = report;
            self.network = None;
        }
        if Instant::now() >= self.next_refresh {
            if let Some(server) = &self.current {
                self.status = cloud9_server::status(&server.root).ok();
                self.logs = tail(&server.root.join("logs/runner.log")).unwrap_or_default();
            }
            self.next_refresh = Instant::now() + Duration::from_millis(750);
        }
        if self.pending.is_some()
            || self.network.is_some()
            || self
                .status
                .as_ref()
                .is_some_and(|status| !matches!(status.phase.as_str(), "Stopped" | "Failed"))
        {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    pub fn view(&mut self, ui: &mut egui::Ui, runner: &Runner) -> Option<PathBuf> {
        let mut import = None;
        ui.heading(RichText::new("Servers").size(32.0));
        ui.add_space(6.0);
        if !self.message.is_empty() {
            ui.colored_label(if self.error { ERROR } else { MUTED }, &self.message);
            ui.add_space(6.0);
        }

        // Matchup import row.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Matchup file").color(TEXT_DIM).small());
            ui.add_space(4.0);
            ui.text_edit_singleline(&mut self.share);
            if ui.button("Import").clicked() {
                import = Some(PathBuf::from(&self.share));
            }
            if ui
                .add_enabled(self.current.is_some(), egui::Button::new("Export"))
                .clicked()
                && let Some(server) = &self.current
            {
                let result = Matchup::from_server(server)
                    .and_then(|share| share.save(Path::new(&self.share)));
                self.report(result, "Matchup exported");
            }
        });
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);

        if self.current.is_some() {
            self.controls(ui);
        } else {
            self.create_form(ui, runner);
        }
        import
    }

    fn create_form(&mut self, ui: &mut egui::Ui, runner: &Runner) {
        if let Some(document) = &runner.selected {
            ui.label(RichText::new(&document.manifest().episode.title).strong());
        } else {
            ui.colored_label(MUTED, "Choose an episode to create its server.");
        }
        ui.add_space(8.0);

        // Server folder.
        ui.horizontal(|ui| {
            ui.label(RichText::new("Server folder").color(TEXT_DIM).small());
            ui.add_space(4.0);
            ui.text_edit_singleline(&mut self.slug);
            if ui
                .add_enabled(self.pending.is_none(), egui::Button::new("Open"))
                .clicked()
            {
                self.open();
            }
        });
        ui.add_space(8.0);

        // Capacity & gameplay.
        ui.horizontal(|ui| {
            ui.add(
                egui::DragValue::new(&mut self.options.port).prefix("Port "),
            );
            ui.add_space(10.0);
            ui.add(
                egui::DragValue::new(&mut self.options.max_players)
                    .range(2..=1000)
                    .prefix("Players "),
            );
            ui.add_space(10.0);
            ui.add(
                egui::DragValue::new(&mut self.options.ram_mib)
                    .range(512..=32768)
                    .speed(64)
                    .suffix(" MiB RAM"),
            );
            ui.add_space(10.0);
            ui.checkbox(&mut self.options.pvp, "PvP");
        });
        ui.add_space(8.0);

        // Authentication & exposure.
        ui.checkbox(&mut self.options.online_mode, "Authenticate players");
        if !self.options.online_mode {
            ui.colored_label(
                ACCENT,
                "Offline mode disables authentication. Keep this server LAN-only.",
            );
            self.options.upnp = false;
            self.options.authorize_public = false;
        }
        ui.add_enabled_ui(self.options.online_mode, |ui| {
            ui.checkbox(&mut self.options.upnp, "Map a public port with UPnP");
            if self.options.upnp {
                ui.colored_label(
                    ACCENT,
                    "This allows outside players to reach your server over the internet.",
                );
                ui.checkbox(
                    &mut self.options.authorize_public,
                    "I authorize internet exposure",
                );
            }
        });
        ui.add_space(8.0);

        // EULA.
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.accept, "I accept the Minecraft server EULA");
            ui.add_space(6.0);
            ui.hyperlink_to("Read EULA", EULA_URL);
        });
        ui.add_space(10.0);

        // Create button.
        if ui
            .add_enabled(
                self.pending.is_none() && runner.selected.is_some(),
                egui::Button::new(RichText::new("Create server").strong().color(GROUND)),
            )
            .fill(ACCENT)
            .clicked()
        {
            self.create(runner);
        }
    }

    fn controls(&mut self, ui: &mut egui::Ui) {
        let Some(server) = self.current.as_ref() else {
            return;
        };
        let root = server.root.clone();
        let active = cloud9_server::active(&root).unwrap_or(false);
        let accepted = server.eula_accepted();
        let online_mode = server.options.online_mode;
        let address = self
            .status
            .as_ref()
            .map_or_else(|| server.address.to_string(), |status| status.address.clone());
        let phase = self
            .status
            .as_ref()
            .map_or("Created", |status| status.phase.as_str());

        // Status badge.
        ui.colored_label(
            if phase == "Running" { OK } else { MUTED },
            format!("{phase} · {address}"),
        );
        ui.add_space(4.0);
        if !online_mode {
            ui.colored_label(ACCENT, "Offline mode disables authentication. LAN only.");
        }
        if !accepted {
            self.eula(ui);
        }
        ui.add_space(8.0);

        // Primary controls — grouped so they don't overflow.
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(
                        !active && self.pending.is_none() && (accepted || self.accept),
                        egui::Button::new("Start"),
                    )
                    .clicked()
                {
                    self.start(root.clone());
                }
                if ui.add_enabled(active, egui::Button::new("Stop")).clicked() {
                    self.report(
                        cloud9_server::request(&root, Control::Stop),
                        "Stopping server",
                    );
                }
                if ui.add_enabled(active, egui::Button::new("Restart")).clicked() {
                    self.report(
                        cloud9_server::request(&root, Control::Restart),
                        "Restart requested",
                    );
                }
                if ui.add_enabled(active, egui::Button::new("Reset round")).clicked() {
                    self.report(
                        cloud9_server::request(&root, Control::Reset),
                        "Round reset requested",
                    );
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Copy join address").clicked() {
                    ui.ctx().copy_text(address.clone());
                }
                ui.add_space(8.0);
                if ui
                    .add_enabled(self.network.is_none(), egui::Button::new("Test TCP"))
                    .clicked()
                {
                    let (send, receive) = channel();
                    self.network = Some(receive);
                    std::thread::spawn(move || {
                        let result = cloud9_server::reachable(&address);
                        let report = result.map_or_else(
                            |error| error.to_string(),
                            |success| {
                                if success {
                                    format!("TCP reachable at {address}")
                                } else {
                                    format!("TCP check could not reach {address}")
                                }
                            },
                        );
                        let _ = send.send(report);
                    });
                    self.message = "Testing TCP reachability".into();
                }
                ui.add_space(8.0);
                if ui
                    .add_enabled(
                        !active && self.pending.is_none(),
                        egui::Button::new("Close"),
                    )
                    .clicked()
                {
                    self.current = None;
                    self.status = None;
                    self.logs.clear();
                    self.message.clear();
                }
            });
        });
        ui.add_space(8.0);

        // Round & players info.
        if let Some(status) = &self.status {
            let players: Vec<_> = status.players.iter().map(String::as_str).collect();
            ui.colored_label(
                MUTED,
                format!(
                    "Round {} · Players ({}) {}",
                    status.round,
                    players.len(),
                    players.join(", ")
                ),
            );
            if let Some(message) = &status.message {
                ui.colored_label(ACCENT, message);
            }
        }
        ui.add_space(10.0);

        // Console command.
        ui.horizontal(|ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.console)
                    .hint_text("Server console command"),
            );
            if ui
                .add_enabled(
                    active && !self.console.is_empty(),
                    egui::Button::new("Send"),
                )
                .clicked()
            {
                let command = std::mem::take(&mut self.console);
                self.report(
                    cloud9_server::request(&root, Control::Command(command)),
                    "Command sent",
                );
            }
        });
        ui.add_space(6.0);

        // Logs.
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
                        ui.label(RichText::new(&self.logs).monospace().size(12.0));
                    });
            });
    }

    fn eula(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.accept, "I accept the Minecraft server EULA");
            ui.add_space(6.0);
            ui.hyperlink_to("Read EULA", EULA_URL);
        });
    }

    fn report(&mut self, result: Result<(), cloud9_server::Error>, success: &str) {
        self.error = result.is_err();
        self.message = result.map_or_else(|error| error.to_string(), |()| success.into());
    }

    fn open(&mut self) {
        if self.slug.is_empty()
            || !self
                .slug
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        {
            self.message = "Enter a server folder name".into();
            self.error = true;
            return;
        }
        match Server::open(&Path::new("servers").join(&self.slug)) {
            Ok(server) => {
                self.current = Some(Box::new(server));
                self.accept = false;
                self.status = None;
                self.error = false;
                self.message.clear();
            }
            Err(error) => {
                self.error = true;
                self.message = error.to_string();
            }
        }
    }

    fn create(&mut self, runner: &Runner) {
        let Some(document) = runner.selected.clone() else {
            return;
        };
        let (mods, java, helper, templates) = runner.paths();
        let slug = self.slug.clone();
        let options = self.options.clone();
        let accept_eula = self.accept;
        self.run_task("Installing Forge server", false, move || {
            let profile = cloud9_mc::Profile::selected()?;
            let server = Server::create(
                &document,
                Create {
                    servers: Path::new("servers"),
                    slug: &slug,
                    cache: Path::new(".cache/minecraft"),
                    templates: &templates,
                    aliases: Path::new("aliases"),
                    local_mods: &mods,
                    helper: &helper,
                    java: java.as_deref(),
                    profile: &profile,
                    options,
                    accept_eula,
                },
            )?;
            Ok(ResultEvent::Opened(Box::new(server)))
        });
    }

    fn start(&mut self, root: PathBuf) {
        let accepted = self.accept;
        self.cancellation.store(false, Ordering::Relaxed);
        let cancel = self.cancellation.clone();
        self.run_task("Preparing the world and starting Forge", true, move || {
            let server = Server::open(&root)?;
            if accepted && !server.eula_accepted() {
                server.accept_eula()?;
            }
            cloud9_server::supervise(&server, &cancel)?;
            Ok(ResultEvent::Finished)
        });
    }

    fn run_task(
        &mut self,
        message: &str,
        supervises: bool,
        work: impl FnOnce() -> Result<ResultEvent, cloud9_server::Error> + Send + 'static,
    ) {
        self.message = message.into();
        self.error = false;
        let (send, receive) = channel();
        self.pending = Some(receive);
        self.supervises = supervises;
        self.worker = Some(std::thread::spawn(move || {
            let result = work().unwrap_or_else(|error| ResultEvent::Error(error.to_string()));
            let _ = send.send(result);
        }));
    }
}

impl Drop for Servers {
    fn drop(&mut self) {
        self.cancellation.store(true, Ordering::Relaxed);
        if self.supervises
            && let Some(worker) = self.worker.take()
        {
            let _ = worker.join();
        }
    }
}

fn tail(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(length.saturating_sub(32 * 1024)))?;
    let mut bytes = Vec::new();
    file.take(32 * 1024).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}
