use crate::{Account, ClientRuntime, Error, Instance, LogEvent, Registry, atomically_write};
use std::{
    fs,
    path::Path,
    process::Child,
    sync::mpsc::{Receiver, channel},
    time::{Duration, Instant},
};

pub fn read_registry(path: &Path, hash: &str) -> Result<Registry, Error> {
    let registry: Registry = serde_json::from_slice(&fs::read(path)?)?;
    registry.check_hash(hash)?;
    Ok(registry)
}

/// Scan the verified mod set once in a disposable local client session.
pub fn bootstrap_client_registry(
    runtime: &ClientRuntime,
    instance: &Instance,
    registry_root: &Path,
    ram_mib: u32,
    account: &Account,
) -> Result<Registry, Error> {
    let cache = registry_root.join(format!("{}.json", instance.mod_set_hash));
    if cache.is_file() {
        return read_registry(&cache, &instance.mod_set_hash);
    }
    if !instance
        .mods
        .iter()
        .any(|entry| entry.mod_id == "runner-helper")
    {
        return Err(Error::Config(
            "runner-helper required for registry discovery".into(),
        ));
    }
    let outfile = instance.root.join("registry_dump.json");
    let (logs, receiver) = channel();
    let mut child = runtime.launch(instance, account, ram_mib, None, logs)?;
    let result = await_registry(&mut child, &receiver, &outfile, &instance.mod_set_hash);
    if child.try_wait()?.is_none() {
        child.kill()?;
        child.wait()?;
    }
    let registry = result?;
    atomically_write(&cache, &serde_json::to_vec(&registry)?)?;
    Ok(registry)
}

pub fn await_registry(
    child: &mut Child,
    receiver: &Receiver<LogEvent>,
    path: &Path,
    hash: &str,
) -> Result<Registry, Error> {
    await_registry_until(child, receiver, path, hash, || false)
}

pub fn await_registry_until(
    child: &mut Child,
    receiver: &Receiver<LogEvent>,
    path: &Path,
    hash: &str,
    cancelled: impl Fn() -> bool,
) -> Result<Registry, Error> {
    let start = Instant::now();
    let mut diagnostic = None;
    while start.elapsed() < Duration::from_secs(120) {
        if cancelled() {
            return Err(Error::Config("registry discovery cancelled".into()));
        }
        if path.is_file() {
            return read_registry(path, hash);
        }
        if let Ok(event) = receiver.recv_timeout(Duration::from_millis(200)) {
            if let Some(message) = event.diagnostic {
                diagnostic = Some(format!(
                    "{} · {}",
                    message.message,
                    message.actions.join(" / ")
                ));
            }
            tracing::debug!(line = %event.line, "registry discovery");
        }
        if let Some(status) = child.try_wait()? {
            return Err(Error::Config(diagnostic.unwrap_or_else(|| {
                format!("Forge exited before registry discovery ({status})")
            })));
        }
    }
    Err(Error::Config(diagnostic.unwrap_or_else(|| {
        "Forge did not produce a registry dump within two minutes".into()
    })))
}
