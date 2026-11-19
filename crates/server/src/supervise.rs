use crate::{Error, Server, json_write, network::Mapping, publish};
use cloud9_mc::{LogEvent, capture_logs};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

const REGISTRATION: &str = "supervisor.json";
const STATUS_FILE: &str = "status.json";

#[derive(Debug, Serialize, Deserialize)]
pub enum Control {
    Stop,
    Restart,
    Reset,
    Command(String),
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Status {
    pub phase: String,
    pub address: String,
    pub players: BTreeSet<String>,
    pub message: Option<String>,
    pub round: u64,
}

#[derive(Deserialize, Serialize)]
struct Registration {
    mailbox: String,
}

pub fn active(root: &Path) -> Result<bool, Error> {
    let file = match OpenOptions::new()
        .read(true)
        .write(true)
        .open(root.join("supervisor.lock"))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    match fs2::FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(true),
        Err(error) => Err(error.into()),
    }
}

pub fn status(root: &Path) -> Result<Status, Error> {
    let mut status: Status = serde_json::from_reader(File::open(root.join(STATUS_FILE))?)?;
    if !active(root)? && status.phase != "Stopped" && status.phase != "Failed" {
        status.phase = "Stopped".into();
        status.players.clear();
        status.message = Some("Supervisor is no longer running".into());
    }
    Ok(status)
}

pub fn request(root: &Path, control: Control) -> Result<(), Error> {
    if !active(root)? {
        return Err(Error::Config("no active server supervisor".into()));
    }
    if let Control::Command(command) = &control
        && (command.trim().is_empty()
            || command.len() > 1024
            || command.contains(['\r', '\n', '\0']))
    {
        return Err(Error::Config(
            "console command must be a single line".into(),
        ));
    }
    let registration: Registration = serde_json::from_reader(File::open(root.join(REGISTRATION))?)?;
    if !registration.mailbox.starts_with(".control-")
        || !registration.mailbox[9..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric())
    {
        return Err(Error::Config("invalid server control folder".into()));
    }
    let directory = root.join(registration.mailbox);
    let file = tempfile::Builder::new()
        .prefix("command-")
        .suffix(".pending")
        .tempfile_in(&directory)?;
    let final_path = file.path().with_extension("json");
    let (file, pending) = file.into_parts();
    drop(file);
    publish(pending.as_ref(), &serde_json::to_vec(&control)?)?;
    fs::rename(&pending, final_path)?;
    Ok(())
}

pub fn supervise(server: &Server, cancelled: &AtomicBool) -> Result<(), Error> {
    let _lock = server.lock()?;
    let mut state = Status {
        phase: "Preparing".into(),
        address: server.address.to_string(),
        players: BTreeSet::new(),
        message: None,
        round: 1,
    };
    let mailbox = tempfile::Builder::new()
        .prefix(".control-")
        .tempdir_in(&server.root)?;
    let name = mailbox
        .path()
        .file_name()
        .and_then(|segment| segment.to_str())
        .ok_or_else(|| Error::Config("invalid control folder name".into()))?;
    json_write(
        &server.root.join(REGISTRATION),
        &Registration {
            mailbox: name.to_owned(),
        },
    )?;
    json_write(&server.root.join(STATUS_FILE), &state)?;
    let result = run(server, &mut state, mailbox.path(), cancelled);
    state.phase = if result.is_ok() { "Stopped" } else { "Failed" }.into();
    state.players.clear();
    state.message = result.as_ref().err().map(ToString::to_string);
    let persisted = json_write(&server.root.join(STATUS_FILE), &state);
    result.and(persisted)
}

fn run(
    server: &Server,
    status: &mut Status,
    mailbox: &Path,
    cancelled: &AtomicBool,
) -> Result<(), Error> {
    server.preflight()?;
    // Detect an orphaned or conflicting process before opening the match save.
    TcpListener::bind(server.address)?;
    let round = match server.round(|| cancelled.load(Ordering::Relaxed)) {
        Ok(round) => round,
        Err(_) if cancelled.load(Ordering::Relaxed) => return Ok(()),
        Err(error) => return Err(error),
    };
    if !round.world.exists() {
        round.prepare()?;
    }
    let mapping = if server.options.upnp {
        let map = Mapping::create(server.address)?;
        status.address = map.address.to_string();
        Some(map)
    } else {
        None
    };
    let mut tunnel = tunnel(server)?;
    if let Some(address) = &server.options.tunnel_address {
        status.address.clone_from(address);
    }
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(server.root.join("logs/runner.log"))?;
    let mut remaining = server.options.auto_restarts;
    let mut renewed = Instant::now();
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(());
        }
        server.preflight()?;
        server.write_properties(
            &server.root,
            server.address,
            crate::WORLD,
            server.options.online_mode,
        )?;
        let mut child = ManagedChild(
            server
                .command()
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let (send, logs) = mpsc::channel();
        if let Some(stdout) = child.0.stdout.take() {
            capture_logs(stdout, send.clone(), Vec::new());
        }
        if let Some(stderr) = child.0.stderr.take() {
            capture_logs(stderr, send, Vec::new());
        }
        status.phase = "Starting".into();
        status.players.clear();
        status.message = None;
        json_write(&server.root.join(STATUS_FILE), status)?;
        let action = monitor(
            server,
            &mut child.0,
            status,
            mailbox,
            &logs,
            &mut log,
            cancelled,
            &mut tunnel,
            &mapping,
            &mut renewed,
        );
        let stopped = stop_child(&mut child.0, Duration::from_secs(20));
        for event in logs.try_iter() {
            writeln!(log, "{}", event.line)?;
        }
        let action = action?;
        stopped?;
        match action {
            End::Stop => return Ok(()),
            End::Reset => {
                round.reset()?;
                status.round += 1;
                remaining = server.options.auto_restarts;
            }
            End::Restart => remaining = server.options.auto_restarts,
            End::Crash(message) if remaining > 0 => {
                remaining -= 1;
                status.phase = "Restarting".into();
                status.message = Some(format!(
                    "{message}. {remaining} automatic retries left after this one."
                ));
                json_write(&server.root.join(STATUS_FILE), status)?;
                for _ in 0..10 {
                    if cancelled.load(Ordering::Relaxed) {
                        return Ok(());
                    }
                    thread::sleep(Duration::from_millis(200));
                }
            }
            End::Crash(message) => return Err(Error::Config(message)),
        }
    }
}

enum End {
    Stop,
    Reset,
    Restart,
    Crash(String),
}

#[allow(clippy::too_many_arguments)]
fn monitor(
    server: &Server,
    child: &mut Child,
    status: &mut Status,
    mailbox: &Path,
    items: &Receiver<LogEvent>,
    log: &mut File,
    cancelled: &AtomicBool,
    tunnel: &mut Option<ManagedChild>,
    mapping: &Option<Mapping>,
    renewed: &mut Instant,
) -> Result<End, Error> {
    let mut latest_diagnostic = None;
    let mut started = false;
    loop {
        if cancelled.load(Ordering::Relaxed) {
            return Ok(End::Stop);
        }
        if let Some(tunnel) = tunnel
            && let Some(exit) = tunnel.0.try_wait()?
        {
            return Err(Error::Config(format!("tunnel provider exited ({exit})")));
        }
        if let Some(mapping) = mapping
            && renewed.elapsed() > Duration::from_secs(900)
        {
            mapping.refresh()?;
            *renewed = Instant::now();
        }
        let mut changed = false;
        for event in items.try_iter().take(1000) {
            writeln!(log, "{}", event.line)?;
            if status.phase == "Starting"
                && event.line.contains("Done (")
                && event.line.contains("For help")
            {
                started = true;
            }
            if let Some((name, joined)) = player_event(&event.line) {
                if joined {
                    status.players.insert(name.to_owned());
                } else {
                    status.players.remove(name);
                }
                changed = true;
            }
            if let Some(diagnostic) = event.diagnostic {
                latest_diagnostic = Some(format!(
                    "{} · {}",
                    diagnostic.message,
                    diagnostic.actions.join(" / ")
                ));
            }
        }
        if started
            && status.phase == "Starting"
            && server
                .root
                .join(crate::WORLD)
                .join("runner_applied.txt")
                .is_file()
        {
            status.phase = "Running".into();
            changed = true;
        }
        if changed {
            json_write(&server.root.join(STATUS_FILE), status)?;
        }
        if let Some(exit) = child.try_wait()? {
            if exit.success() && status.phase == "Running" {
                return Ok(End::Stop);
            }
            return Ok(End::Crash(
                latest_diagnostic.unwrap_or_else(|| format!("Forge server exited ({exit})")),
            ));
        }
        for path in messages(mailbox)? {
            let message: Control = serde_json::from_reader(File::open(&path)?)?;
            fs::remove_file(path)?;
            let action = match message {
                Control::Stop => Some(End::Stop),
                Control::Reset => Some(End::Reset),
                Control::Restart => Some(End::Restart),
                Control::Command(command) if command.trim() == "stop" => Some(End::Stop),
                Control::Command(command) => {
                    send(child, &command)?;
                    None
                }
            };
            if let Some(action) = action {
                status.phase = "Stopping".into();
                json_write(&server.root.join(STATUS_FILE), status)?;
                return Ok(action);
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn messages(mailbox: &Path) -> Result<Vec<PathBuf>, Error> {
    let mut paths = Vec::new();
    for file in fs::read_dir(mailbox)? {
        let path = file?.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            paths.push(path);
        }
    }
    paths.sort_by_key(|path| fs::metadata(path).and_then(|meta| meta.modified()).ok());
    Ok(paths)
}

fn player_event(line: &str) -> Option<(&str, bool)> {
    let text = line
        .split("CDATA[")
        .last()?
        .trim_end_matches("]]></log4j:Message>");
    let text = text.trim_end_matches([']', '>']);
    for (ending, joined) in [(" joined the game", true), (" left the game", false)] {
        if let Some(before) = text.strip_suffix(ending) {
            let name = before.split_whitespace().last()?;
            if !before.contains('<')
                && (1..=16).contains(&name.len())
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                return Some((name, joined));
            }
        }
    }
    None
}

fn send(child: &mut Child, command: &str) -> Result<(), Error> {
    let stdin = child
        .stdin
        .as_mut()
        .ok_or_else(|| Error::Config("server stdin is closed".into()))?;
    writeln!(stdin, "{command}")?;
    stdin.flush()?;
    Ok(())
}

pub(crate) fn stop_child(child: &mut Child, timeout: Duration) -> Result<(), Error> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    let _ = send(child, "stop");
    let until = Instant::now() + timeout;
    while Instant::now() < until {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(150));
    }
    tracing::warn!(pid = child.id(), "server stop timed out");
    child.kill()?;
    child.wait()?;
    Ok(())
}

struct ManagedChild(Child);
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if self.0.try_wait().is_ok_and(|state| state.is_none()) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn tunnel(server: &Server) -> Result<Option<ManagedChild>, Error> {
    let Some((executable, args)) = server.options.tunnel.split_first() else {
        return Ok(None);
    };
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(server.root.join("logs/tunnel.log"))?;
    let child = std::process::Command::new(executable)
        .args(args)
        .env("CLOUD9_LOCAL_ADDRESS", server.address.to_string())
        .current_dir(&server.root)
        .stdin(Stdio::null())
        .stdout(file.try_clone()?)
        .stderr(file)
        .spawn()?;
    Ok(Some(ManagedChild(child)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graceful_stop_writes_stdin_and_waits() -> Result<(), Box<dyn std::error::Error>> {
        #[cfg(windows)]
        let mut command = {
            let mut cmd = std::process::Command::new("cmd");
            cmd.args(["/D", "/C", "set /p c="]);
            cmd
        };
        #[cfg(not(windows))]
        let mut command = {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-c", "read line && test \"$line\" = stop"]);
            cmd
        };
        let mut child = command.stdin(Stdio::piped()).spawn()?;
        stop_child(&mut child, Duration::from_secs(3))?;
        assert!(child.try_wait()?.is_some_and(|code| code.success()));
        Ok(())
    }

    #[test]
    fn detects_player_events_and_ignores_chat() {
        assert_eq!(
            player_event("[Server thread/INFO]: Alex joined the game"),
            Some(("Alex", true))
        );
        assert_eq!(
            player_event("    <log4j:Message><![CDATA[Alex left the game]]></log4j:Message>"),
            Some(("Alex", false))
        );
        assert_eq!(
            player_event("[Server thread/INFO]: <Alex> Hacker joined the game"),
            None
        );
    }
}
