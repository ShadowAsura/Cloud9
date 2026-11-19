use clap::{Parser, Subcommand};
use cloud9_core::Document;
use cloud9_mc::Profile;
use cloud9_server::{
    Control, Create, Matchup, Options, Server, active, request, status, supervise,
};
use serde::Deserialize;
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[derive(Parser)]
#[command(
    name = "runner-cli",
    about = "Run Cloud 9 episodes on a dedicated Forge server"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    CachePrune {
        #[arg(long, default_value = ".cache/minecraft/mods")]
        directory: PathBuf,
        #[arg(long, default_value_t = 4096)]
        max_mib: u64,
        #[arg(long, default_value_t = 90)]
        max_days: u64,
    },
    Bundle {
        instance: PathBuf,
        destination: PathBuf,
    },
    Server {
        #[arg(long, global = true, default_value = "server.toml")]
        config: PathBuf,
        #[command(subcommand)]
        action: Action,
    },
}

#[derive(Subcommand)]
enum Action {
    Create,
    Start,
    Stop,
    Restart,
    Reset,
    Logs {
        #[arg(long)]
        follow: bool,
    },
    Status,
    Console {
        command: String,
    },
    Export {
        path: PathBuf,
    },
}

#[derive(Deserialize)]
struct Config {
    manifest: PathBuf,
    slug: String,
    servers: PathBuf,
    cache: PathBuf,
    templates: PathBuf,
    aliases: PathBuf,
    local_mods: PathBuf,
    helper: PathBuf,
    java: Option<PathBuf>,
    profile: Option<PathBuf>,
    #[serde(default)]
    accept_eula: bool,
    #[serde(default)]
    settings: Options,
}

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(io::stderr)
        .init();
    match run(Cli::parse()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let (config, action) = match cli.command {
        Command::CachePrune {
            directory,
            max_mib,
            max_days,
        } => {
            let removed = cloud9_mc::prune_mod_cache(
                &directory,
                max_mib.saturating_mul(1024 * 1024),
                Duration::from_secs(max_days.saturating_mul(86400)),
            )?;
            println!("Removed {removed} cached mod file(s)");
            return Ok(());
        }
        Command::Bundle {
            instance,
            destination,
        } => {
            let count = cloud9_mc::support_bundle(&instance, &destination)?;
            println!(
                "Wrote {} with {count} log excerpt(s)",
                destination.display()
            );
            return Ok(());
        }
        Command::Server { config, action } => (config, action),
    };
    let base = fs::canonicalize(&config)?;
    let parent = base
        .parent()
        .ok_or("configuration needs a parent directory")?;
    let config: Config = toml::from_str(&fs::read_to_string(&base)?)?;
    let server_root = parent.join(&config.servers).join(&config.slug);
    match action {
        Action::Create => {
            let document = Document::load(&parent.join(config.manifest))?;
            let profile = match &config.profile {
                Some(path) => toml::from_str(&fs::read_to_string(parent.join(path))?)?,
                None => Profile::primary()?,
            };
            if !config.settings.online_mode {
                eprintln!("Offline mode disables authentication. Keep this server LAN-only.");
            }
            if config.settings.authorize_public {
                eprintln!("Internet exposure allows outside players to reach this server.");
            }
            let server = Server::create(
                &document,
                Create {
                    servers: &parent.join(config.servers),
                    slug: &config.slug,
                    cache: &parent.join(config.cache),
                    templates: &parent.join(config.templates),
                    aliases: &parent.join(config.aliases),
                    local_mods: &parent.join(config.local_mods),
                    helper: &parent.join(config.helper),
                    java: config
                        .java
                        .as_ref()
                        .map(|path| parent.join(path))
                        .as_deref(),
                    profile: &profile,
                    options: config.settings,
                    accept_eula: config.accept_eula,
                },
            )?;
            println!("Created {} at {}", server.root.display(), server.address);
            if !server.eula_accepted() {
                println!(
                    "Set accept_eula = true after reading {}",
                    cloud9_server::EULA_URL
                );
            }
        }
        Action::Start => {
            let server = Server::open(&server_root)?;
            if config.accept_eula && !server.eula_accepted() {
                server.accept_eula()?;
            }
            let cancelled = cancellation()?;
            println!("Starting {}", server.address);
            supervise(&server, &cancelled)?;
        }
        Action::Stop => {
            request(&server_root, Control::Stop)?;
            wait(&server_root, |running| Ok(!running))?;
        }
        Action::Restart => {
            request(&server_root, Control::Restart)?;
        }
        Action::Reset => {
            let previous = status(&server_root)?.round;
            request(&server_root, Control::Reset)?;
            wait(&server_root, |running| {
                if !running {
                    return Err("server stopped while resetting".into());
                }
                let current = status(&server_root)?;
                Ok(current.round > previous && current.phase == "Running")
            })?;
        }
        Action::Logs { follow } => {
            let path = server_root.join("logs/runner.log");
            if follow {
                follow_log(&path)?;
            } else {
                std::io::copy(&mut File::open(path)?, &mut io::stdout())?;
            }
        }
        Action::Status => {
            let current = status(&server_root)?;
            println!(
                "{} at {} · {} player(s) · round {}",
                current.phase,
                current.address,
                current.players.len(),
                current.round
            );
            if let Some(message) = current.message {
                println!("{message}");
            }
            for player in &current.players {
                println!("{player}");
            }
        }
        Action::Console { command } => request(&server_root, Control::Command(command))?,
        Action::Export { path } => {
            Matchup::from_server(&Server::open(&server_root)?)?.save(&path)?;
            println!("Wrote {}", path.display());
        }
    }
    Ok(())
}

fn cancellation() -> Result<Arc<AtomicBool>, ctrlc::Error> {
    let cancelled = Arc::new(AtomicBool::new(false));
    let signal = cancelled.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    Ok(cancelled)
}

fn wait(
    root: &Path,
    mut condition: impl FnMut(bool) -> Result<bool, Box<dyn std::error::Error>>,
) -> Result<(), Box<dyn std::error::Error>> {
    let until = Instant::now() + Duration::from_secs(120);
    while Instant::now() < until {
        if condition(active(root)?)? {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(200));
    }
    Err("timed out waiting for the server".into())
}

fn follow_log(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let cancelled = cancellation()?;
    let mut file = File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(16 * 1024);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = [0; 8192];
    while !cancelled.load(Ordering::Relaxed) {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            thread::sleep(Duration::from_millis(200));
        } else {
            io::stdout().write_all(&bytes[..count])?;
            io::stdout().flush()?;
        }
    }
    Ok(())
}
