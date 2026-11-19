use crate::Error;
use regex::Regex;
use std::{
    fs::{self, File},
    io::{self, Read, Seek, SeekFrom, Write},
    path::Path,
    time::{Duration, SystemTime},
};
use zip::{ZipWriter, write::SimpleFileOptions};

const LOG_TAIL: u64 = 96 * 1024;

/// Export only selected log tails. The archive stays local and excludes
/// worlds, manifests, lockfiles, account data, and game binaries.
pub fn support_bundle(instance: &Path, destination: &Path) -> Result<usize, Error> {
    let parent = destination
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut archive = tempfile::NamedTempFile::new_in(parent)?;
    let secrets = Regex::new(r#"(?i)(?P<label>(?:--accessToken|access_token|authorization)[\s"':=]+(?:Bearer\s+)?)[^\s,'";]+"#)
        .map_err(|error| Error::Config(error.to_string()))?;
    let addresses = Regex::new(r"\b(?:\d{1,3}\.){3}\d{1,3}(?::\d{1,5})?\b")
        .map_err(|error| Error::Config(error.to_string()))?;
    let mut paths = vec![
        instance.to_string_lossy().into_owned(),
        instance.to_string_lossy().replace('\\', "/"),
    ];
    for key in ["USERPROFILE", "HOME"] {
        if let Some(path) = std::env::var_os(key) {
            paths.push(path.to_string_lossy().into_owned());
            paths.push(path.to_string_lossy().replace('\\', "/"));
        }
    }
    paths.sort_by_key(|path| std::cmp::Reverse(path.len()));
    paths.dedup();
    let mut logs = 0;
    {
        let mut zip = ZipWriter::new(archive.as_file_mut());
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for name in [
            "runner.log",
            "latest.log",
            "fml-client-latest.log",
            "fml-server-latest.log",
            "installer.log",
        ] {
            let path = instance.join("logs").join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_file() => {}
                Ok(_) => continue,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            }
            let mut file = File::open(path)?;
            let start = file.metadata()?.len().saturating_sub(LOG_TAIL);
            file.seek(SeekFrom::Start(start))?;
            let mut bytes = Vec::new();
            file.take(LOG_TAIL).read_to_end(&mut bytes)?;
            let value = String::from_utf8_lossy(&bytes);
            let mut sanitized = secrets
                .replace_all(&value, "${label}[redacted]")
                .into_owned();
            sanitized = addresses.replace_all(&sanitized, "[address]").into_owned();
            for path in paths.iter().filter(|path| !path.is_empty()) {
                sanitized = sanitized.replace(path, "[local path]");
            }
            zip.start_file(format!("logs/{name}"), options)?;
            zip.write_all(sanitized.as_bytes())?;
            logs += 1;
        }
        if logs == 0 {
            return Err(Error::Config(
                "no local run logs were available to bundle".into(),
            ));
        }
        zip.start_file("system.txt", options)?;
        writeln!(
            zip,
            "Cloud 9 {}\n{} {}",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH
        )?;
        writeln!(
            zip,
            "Review the log excerpts before sharing. They may contain player names and mod output."
        )?;
        zip.finish()?;
    }
    archive.as_file().sync_all()?;
    archive.persist(destination).map_err(|error| error.error)?;
    Ok(logs)
}

/// Remove expired or oldest mod cache files after accounting for the size
/// budget. Existing instances own copies of their jars.
pub fn prune_mod_cache(path: &Path, max_bytes: u64, max_age: Duration) -> Result<usize, Error> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error.into()),
    };
    let mut total = 0u64;
    let mut jars = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let path = entry.path();
        if !path
            .file_stem()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.len() == 64 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
            || path.extension().is_none_or(|ext| ext != "jar")
        {
            continue;
        }
        let meta = entry.metadata()?;
        total = total.saturating_add(meta.len());
        jars.push((
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            meta.len(),
            path,
        ));
    }
    jars.sort_unstable();
    let mut removed = 0;
    for (modified, size, path) in jars {
        if total <= max_bytes && modified.elapsed().is_ok_and(|age| age <= max_age) {
            break;
        }
        match fs::remove_file(&path) {
            Ok(()) => {
                total = total.saturating_sub(size);
                removed += 1;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                total = total.saturating_sub(size);
            }
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                tracing::debug!(%error, path = %path.display(), "mod cache entry busy");
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_redacts_tokens_and_ignores_unlisted_files() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        fs::create_dir(root.path().join("logs"))?;
        fs::write(
            root.path().join("logs/runner.log"),
            "access_token=example-secret\n127.0.0.1:25565",
        )?;
        fs::write(root.path().join("server.lock.json"), "private lockfile")?;
        let target = root.path().join("report.zip");
        assert_eq!(support_bundle(root.path(), &target)?, 1);
        let mut zip = zip::ZipArchive::new(File::open(target)?)?;
        assert_eq!(zip.len(), 2);
        let mut output = String::new();
        zip.by_name("logs/runner.log")?
            .read_to_string(&mut output)?;
        assert!(output.contains("[redacted]"));
        assert!(!output.contains("example-secret"));
        assert!(!output.contains("127.0.0.1"));
        Ok(())
    }
}
