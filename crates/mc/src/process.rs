use std::{
    io::{self, BufRead, BufReader, Read},
    sync::mpsc::Sender,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Diagnostic {
    pub message: &'static str,
    pub actions: &'static [&'static str],
}

#[derive(Debug, Clone)]
pub struct LogEvent {
    pub level: Level,
    pub line: String,
    pub diagnostic: Option<Diagnostic>,
}

pub fn diagnose(line: &str) -> Option<Diagnostic> {
    let line = line.to_ascii_lowercase();
    let (message, actions): (&str, &[&str]) =
        if line.contains("outofmemory") || line.contains("java heap space") {
            ("Java ran out of memory", &["Increase RAM"])
        } else if line.contains("unsupportedclassversionerror")
            || line.contains("unrecognized vm option")
        {
            ("Wrong Java version", &["Locate Java"])
        } else if line.contains("missingmods")
            || line.contains("missing dependency")
            || line.contains("requires mods")
        {
            ("Missing mod dependency", &["Locate jar", "Fetch"])
        } else if line.contains("unknown entity")
            || line.contains("unknown item")
            || line.contains("unknown block")
        {
            ("Unresolved registry entry", &["Edit alias", "Ignore"])
        } else if line.contains("modloadingexception") || line.contains("loaderexception") {
            ("Forge failed to load a mod", &["Locate jar", "Fetch"])
        } else {
            return None;
        };
    Some(Diagnostic { message, actions })
}

/// Capture a process pipe on a worker thread and redact any supplied secrets before emitting events.
pub fn capture_logs(
    reader: impl Read + Send + 'static,
    sender: Sender<LogEvent>,
    secrets: Vec<String>,
) -> std::thread::JoinHandle<io::Result<()>> {
    std::thread::spawn(move || {
        let mut reader = BufReader::new(reader);
        let mut bytes = Vec::new();
        while reader.read_until(b'\n', &mut bytes)? != 0 {
            let mut line = String::from_utf8_lossy(&bytes).trim_end().to_owned();
            bytes.clear();
            for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
                line = line.replace(secret, "[redacted]");
            }
            let diagnostic = diagnose(&line);
            let level =
                if line.contains("/ERROR]") || line.contains("/FATAL]") || diagnostic.is_some() {
                    Level::Error
                } else if line.contains("/WARN]") {
                    Level::Warn
                } else {
                    Level::Info
                };
            if sender
                .send(LogEvent {
                    level,
                    line,
                    diagnostic,
                })
                .is_err()
            {
                // Keep draining the pipe even when the UI no longer tracks it.
                continue;
            }
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_and_redacts_output() {
        let (tx, rx) = std::sync::mpsc::channel();
        capture_logs(
            &b"[main/ERROR] MissingModsException with secret\n"[..],
            tx,
            vec!["secret".into()],
        )
        .join()
        .expect("worker")
        .expect("read");
        let event = rx.recv().expect("event");
        assert_eq!(event.level, Level::Error);
        assert!(!event.line.contains("secret"));
        assert_eq!(
            event.diagnostic.expect("diagnostic").actions,
            &["Locate jar", "Fetch"]
        );
    }
}
