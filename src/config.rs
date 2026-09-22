use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub host: IpAddr,
    pub port: u16,
    pub codex: Option<PathBuf>,
    pub timeout: u64,
    pub max_concurrency: usize,
    pub api_key: String,
}

pub fn token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            host: Ipv4Addr::LOCALHOST.into(),
            port: 8787,
            codex: None,
            timeout: 300,
            max_concurrency: 2,
            api_key: format!("sk-gprox-{}", token()),
        }
    }
}

impl Settings {
    pub fn load(home: &Path) -> Result<Self> {
        let path = home.join("config.json");
        if path.exists() {
            let settings =
                serde_json::from_slice(&fs::read(&path)?).context("Invalid gprox config.json")?;
            Ok(settings)
        } else {
            let settings = Self::default();
            settings.save(home)?;
            Ok(settings)
        }
    }

    pub fn save(&self, home: &Path) -> Result<()> {
        write_json(&home.join("config.json"), self)
    }

    pub fn apply(&mut self, args: crate::StartArgs) -> Result<()> {
        if let Some(v) = args.host {
            self.host = v;
        }
        if let Some(v) = args.port {
            self.port = v;
        }
        if let Some(v) = args.codex {
            self.codex = Some(fs::canonicalize(v)?);
        }
        if let Some(v) = args.timeout {
            self.timeout = v;
        }
        if let Some(v) = args.max_concurrency {
            self.max_concurrency = v;
        }
        if self.port == 0 || self.timeout == 0 || self.max_concurrency == 0 {
            bail!("port, timeout and max-concurrency must be greater than zero");
        }
        if self.api_key.len() < 32
            || !self.api_key.is_ascii()
            || self.api_key.chars().any(char::is_whitespace)
        {
            bail!("api_key must contain at least 32 ASCII characters without whitespace");
        }
        Ok(())
    }
}

pub fn home(path: Option<PathBuf>) -> Result<PathBuf> {
    let path = path.unwrap_or(
        dirs::home_dir()
            .context("Cannot find home directory")?
            .join(".gprox"),
    );
    fs::create_dir_all(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    fs::canonicalize(&path).context("Cannot resolve gprox home")
}

pub fn lock(home: &Path, name: &str) -> Result<File> {
    let file = private_file(&home.join(name), false)?;
    file.try_lock_exclusive()
        .context("Another gprox instance is running or updating settings")?;
    Ok(file)
}

pub fn private_file(path: &Path, truncate: bool) -> Result<File> {
    let mut options = OpenOptions::new();
    options
        .create(true)
        .read(true)
        .write(true)
        .truncate(truncate);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    use std::io::Write;
    let bytes = serde_json::to_vec_pretty(value)?;
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file = private_file(&temporary, true)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("Cannot atomically save gprox state");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn credentials_persist_and_lock_excludes_other_instances() {
        let dir = tempfile::tempdir().unwrap();
        let one = Settings::load(dir.path()).unwrap();
        assert_eq!(one.api_key, Settings::load(dir.path()).unwrap().api_key);
        let held = lock(dir.path(), "run.lock").unwrap();
        assert!(lock(dir.path(), "run.lock").is_err());
        drop(held);
        assert!(lock(dir.path(), "run.lock").is_ok());
    }
}
