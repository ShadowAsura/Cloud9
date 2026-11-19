use crate::{Error, Installer, Profile, artifact::verify_sha1, atomically_write, discover_java};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Serialize, Deserialize)]
pub struct ServerRuntime {
    forge_jar: String,
    logging: String,
    files: Vec<VerifiedFile>,
}

#[derive(Debug, Serialize, Deserialize)]
struct VerifiedFile {
    path: PathBuf,
    sha1: String,
}

impl ServerRuntime {
    /// Run the verified Forge installer headlessly in an isolated directory.
    pub fn install(
        installer: &Installer,
        profile: &Profile,
        java: &Path,
        root: &Path,
    ) -> Result<Self, Error> {
        discover_java(profile, Some(java))?;
        let vanilla = installer.vanilla(profile)?;
        let (forge, universal) = installer.legacy_forge(profile)?;
        let vanilla_name = format!("minecraft_server.{}.jar", profile.mc_version);
        let vanilla_jar = root.join(&vanilla_name);
        installer.download_entry(&vanilla["downloads"]["server"], &vanilla_jar)?;
        let mut files = vec![verified(&vanilla_jar, root)?];

        // Use the verified profile to pre-stage dependencies. Legacy installers
        // otherwise fall back to retired HTTP artifact endpoints.
        for path in installer.libraries(&[&forge], &universal, &root.join("natives"))? {
            if path == universal {
                continue;
            }
            let relative = path
                .strip_prefix(&installer.root)
                .map_err(|_| Error::Config("Forge dependency outside cache".into()))?;
            let target = root.join(relative);
            atomically_write(&target, &fs::read(path)?)?;
            files.push(verified(&target, root)?);
        }
        let executable = installer.forge_installer(profile)?;
        let result = Command::new(java)
            .current_dir(root)
            .arg("-Djava.awt.headless=true")
            .arg("-jar")
            .arg(executable)
            .arg("--installServer")
            .output()?;
        let mut log = result.stdout;
        log.extend(result.stderr);
        atomically_write(&root.join("logs/installer.log"), &log)?;
        let forge_jar = format!("forge-{}-universal.jar", profile.forge_coordinate);
        let output = root.join(&forge_jar);
        if !result.status.success() || !verify_sha1(&output, &[&profile.universal_sha1])? {
            let lines = String::from_utf8_lossy(&log);
            let tail: Vec<_> = lines.lines().rev().take(6).collect();
            return Err(Error::Config(format!(
                "Forge server installer did not produce a verified jar ({})",
                tail.into_iter().rev().collect::<Vec<_>>().join(" / ")
            )));
        }
        files.push(verified(&output, root)?);
        let logging = "log4j2_17-111.xml".to_owned();
        let config = root.join(&logging);
        installer.get(
            "https://launcher.mojang.com/v1/objects/4bb89a97a66f350bc9f73b3ca8509632682aea2e/log4j2_17-111.xml",
            &["4bb89a97a66f350bc9f73b3ca8509632682aea2e"], &config,
        )?;
        files.push(verified(&config, root)?);
        let runtime = Self {
            forge_jar,
            logging,
            files,
        };
        runtime.verify(root)?;
        Ok(runtime)
    }

    pub fn verify(&self, root: &Path) -> Result<(), Error> {
        if !crate::safe_component(&self.forge_jar)
            || !crate::safe_component(&self.logging)
            || [&self.forge_jar, &self.logging].iter().any(|name| {
                !self
                    .files
                    .iter()
                    .any(|file| file.path.as_os_str() == name.as_str())
            })
        {
            return Err(Error::Config("invalid server runtime lock".into()));
        }
        for file in &self.files {
            if file
                .path
                .components()
                .any(|segment| !matches!(segment, std::path::Component::Normal(_)))
            {
                return Err(Error::Config("invalid server runtime file path".into()));
            }
            let path = root.join(&file.path);
            if !verify_sha1(&path, &[&file.sha1])? {
                return Err(Error::Hash(path));
            }
        }
        Ok(())
    }

    pub fn command(&self, root: &Path, java: &Path, hash: &str, ram_mib: u32) -> Command {
        let mut process = Command::new(java);
        process
            .current_dir(root)
            .arg(format!("-Xmx{ram_mib}M"))
            .arg(format!("-Dcloud9.modSetHash={hash}"))
            .arg(format!("-Dlog4j.configurationFile={}", self.logging))
            .arg("-Djava.awt.headless=true")
            .arg("-jar")
            .arg(&self.forge_jar)
            .arg("nogui");
        process
    }
}

fn verified(path: &Path, root: &Path) -> Result<VerifiedFile, Error> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| Error::Config("server artifact outside instance".into()))?;
    let mut hash = Sha1::new();
    io::copy(&mut File::open(path)?, &mut hash)?;
    Ok(VerifiedFile {
        path: relative.into(),
        sha1: format!("{:x}", hash.finalize()),
    })
}
