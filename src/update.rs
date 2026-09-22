//! Release contract: Cargo version X.Y.Z, tag vX.Y.Z, a Windows ZIP and SHA256SUMS.
use crate::{StartArgs, config, service};
use anyhow::{Context, Result, bail, ensure};
use reqwest::{Client, StatusCode};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command};

const REPOSITORY: &str = "Ziiilk/gprox";
const TARGET: &str = "x86_64-pc-windows-msvc";
const ASSET_NAME: &str = "gprox-x86_64-pc-windows-msvc.zip";
const CHECKSUM_NAME: &str = "SHA256SUMS";
const MAX_ARCHIVE_SIZE: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

impl Release {
    fn version(&self) -> Result<Version> {
        ensure!(
            !self.draft && !self.prerelease,
            "Only stable releases can be installed"
        );
        let version = self
            .tag_name
            .strip_prefix('v')
            .context("Release tag must start with v")?;
        let version = Version::parse(version).context("Invalid release version")?;
        ensure!(
            version.pre.is_empty() && version.build.is_empty(),
            "Release tag must be vMAJOR.MINOR.PATCH"
        );
        Ok(version)
    }

    fn asset_url(&self, name: &str) -> Result<&str> {
        let mut matching = self.assets.iter().filter(|asset| asset.name == name);
        let asset = matching
            .next()
            .with_context(|| format!("Release {} is missing {name}", self.tag_name))?;
        ensure!(matching.next().is_none(), "Duplicate release asset: {name}");
        let expected = format!(
            "https://github.com/{REPOSITORY}/releases/download/{}/{name}",
            self.tag_name
        );
        ensure!(
            asset.browser_download_url == expected,
            "Unexpected download URL for {name}"
        );
        Ok(&asset.browser_download_url)
    }
}

struct Github {
    client: Client,
    api_base: String,
}

impl Github {
    fn new() -> Result<Self> {
        Ok(Self {
            client: Client::builder()
                .user_agent(concat!("gprox/", env!("CARGO_PKG_VERSION")))
                .connect_timeout(Duration::from_secs(15))
                .timeout(Duration::from_secs(180))
                .build()?,
            api_base: "https://api.github.com".into(),
        })
    }

    async fn latest(&self) -> Result<Option<Release>> {
        self.latest_with_token_source(gh_token).await
    }

    async fn latest_with_token_source<F, Fut>(&self, token_source: F) -> Result<Option<Release>>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<String>>,
    {
        let url = format!("{}/repos/{REPOSITORY}/releases/latest", self.api_base);
        let request = || {
            self.client
                .get(&url)
                .header("Accept", "application/vnd.github+json")
        };
        let mut response = request()
            .send()
            .await
            .context("Cannot check GitHub releases")?;
        // Request credentials only when the anonymous API quota is exhausted.
        if matches!(
            response.status(),
            StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS
        ) {
            eprintln!("GitHub anonymous API limit reached; retrying with gh credentials");
            let token = token_source().await.context("Run `gh auth login --hostname github.com` or retry after the GitHub rate limit resets")?;
            response = request().bearer_auth(token).send().await?;
        }
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        let response = response
            .error_for_status()
            .context("GitHub release lookup failed")?;
        let body = read_limited(response, 2 * 1024 * 1024).await?;
        Ok(Some(
            serde_json::from_slice(&body).context("Invalid GitHub release response")?,
        ))
    }

    async fn stage(&self, release: &Release, version: &Version) -> Result<StagedUpdate> {
        let archive_url = release.asset_url(ASSET_NAME)?;
        let checksum_url = release.asset_url(CHECKSUM_NAME)?;
        let checksums = self
            .client
            .get(checksum_url)
            .send()
            .await?
            .error_for_status()?;
        let checksums = read_limited(checksums, 16 * 1024).await?;
        let expected_hash = checksum_for(std::str::from_utf8(&checksums)?, ASSET_NAME)?;
        let directory = self_update::TempDir::new()?;
        let archive = directory.path().join(ASSET_NAME);
        println!("Downloading {ASSET_NAME}...");
        download_archive(&self.client, archive_url, &archive, &expected_hash).await?;
        let extract_dir = directory.path().to_owned();
        tokio::task::spawn_blocking(move || {
            self_update::Extract::from_source(&archive).extract_file(&extract_dir, "gprox.exe")
        })
        .await
        .context("Archive extraction task failed")??;
        let executable = directory.path().join("gprox.exe");
        verify_binary(&executable, version).await?;
        Ok(StagedUpdate {
            _directory: directory,
            executable,
        })
    }
}

struct StagedUpdate {
    _directory: self_update::TempDir,
    executable: PathBuf,
}

async fn gh_token() -> Result<String> {
    let mut command = Command::new("gh");
    command
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .context("gh auth token timed out")?
        .context("GitHub CLI is unavailable")?;
    ensure!(
        output.status.success(),
        "GitHub CLI has no usable github.com login"
    );
    let token = String::from_utf8(output.stdout)?.trim().to_owned();
    ensure!(!token.is_empty(), "GitHub CLI returned an empty token");
    Ok(token)
}

async fn read_limited(mut response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "Release response exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn checksum_for(contents: &str, name: &str) -> Result<String> {
    let mut found = None;
    for line in contents.lines().filter(|line| !line.trim().is_empty()) {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() == 2 && fields[1].trim_start_matches('*') == name {
            ensure!(found.is_none(), "Duplicate checksum for {name}");
            ensure!(
                fields[0].len() == 64 && fields[0].bytes().all(|b| b.is_ascii_hexdigit()),
                "Invalid SHA256 checksum for {name}"
            );
            found = Some(fields[0].to_ascii_lowercase());
        }
    }
    found.with_context(|| format!("SHA256SUMS has no checksum for {name}"))
}

async fn download_archive(client: &Client, url: &str, path: &Path, expected: &str) -> Result<()> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    ensure!(
        response.content_length().unwrap_or(0) <= MAX_ARCHIVE_SIZE,
        "Release archive is too large"
    );
    let mut file = tokio::fs::File::create(path).await?;
    let mut hasher = Sha256::new();
    let mut length = 0_u64;
    while let Some(chunk) = response.chunk().await? {
        length += chunk.len() as u64;
        ensure!(length <= MAX_ARCHIVE_SIZE, "Release archive is too large");
        hasher.update(&chunk);
        file.write_all(&chunk).await?;
    }
    file.flush().await?;
    ensure!(
        format!("{:x}", hasher.finalize()) == expected,
        "SHA256 mismatch; installed executable was not changed"
    );
    Ok(())
}

async fn verify_binary(executable: &Path, version: &Version) -> Result<()> {
    let mut command = Command::new(executable);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .context("Downloaded executable version check timed out")??;
    ensure!(
        output.status.success()
            && String::from_utf8_lossy(&output.stdout).trim() == format!("gprox {version}"),
        "Downloaded executable version does not match release v{version}"
    );
    Ok(())
}

pub fn print_version(json: bool) {
    if json {
        println!(
            "{}",
            serde_json::json!({
                "name": env!("CARGO_PKG_NAME"), "version": env!("CARGO_PKG_VERSION"),
                "description": env!("CARGO_PKG_DESCRIPTION"), "repository": env!("CARGO_PKG_REPOSITORY"),
            })
        );
    } else {
        println!("gprox {}", env!("CARGO_PKG_VERSION"));
    }
}

pub async fn execute(home: Option<PathBuf>, check: bool) -> Result<()> {
    ensure!(
        cfg!(all(windows, target_arch = "x86_64", target_env = "msvc")),
        "Release updates currently support {TARGET} only; use cargo install on this platform"
    );
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    println!("Current version: {current}");
    let github = Github::new()?;
    let Some(release) = github.latest().await? else {
        println!("No stable releases published yet: https://github.com/{REPOSITORY}/releases");
        return Ok(());
    };
    let latest = release.version()?;
    if latest <= current {
        println!("Already up to date (latest stable: {latest})");
        return Ok(());
    }
    release.asset_url(ASSET_NAME)?;
    release.asset_url(CHECKSUM_NAME)?;
    println!("Update available: {current} -> {latest}");
    if check {
        return Ok(());
    }

    let home = config::home(home)?;
    let _update_lock = config::lock(&home, "update.lock")?;
    let executable = std::env::current_exe()?;
    let staged = github.stage(&release, &latest).await?;
    let was_running = service::running_for_update(&home).await?;
    if was_running {
        println!("Restarting the active proxy in the background after installation...");
        service::stop(&home).await?;
    }
    // Retain the original install path: current_exe() can change after replacement.
    let install_result = self_update::self_replace::self_replace(&staged.executable);
    let restart_result = if was_running {
        service::start_background_with_executable(&home, StartArgs::default(), &executable).await
    } else {
        Ok(())
    };
    match (install_result, restart_result) {
        (Ok(()), Ok(())) => println!("Updated to version {latest}"),
        (Ok(()), Err(error)) => bail!("Updated to {latest}, but proxy restart failed: {error:#}"),
        (Err(error), Ok(())) => return Err(error).context("Executable replacement failed"),
        (Err(error), Err(restart)) => {
            bail!("Executable replacement failed: {error}; proxy restart failed: {restart:#}")
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::HeaderMap, routing::get};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn release(tag: &str) -> Release {
        Release {
            tag_name: tag.into(),
            draft: false,
            prerelease: false,
            assets: vec![ASSET_NAME, CHECKSUM_NAME]
                .into_iter()
                .map(|name| Asset {
                    name: name.into(),
                    browser_download_url: format!(
                        "https://github.com/{REPOSITORY}/releases/download/{tag}/{name}"
                    ),
                })
                .collect(),
        }
    }

    #[test]
    fn release_contract_rejects_ambiguous_versions_and_assets() {
        assert_eq!(
            release("v0.1.30").version().unwrap(),
            Version::new(0, 1, 30)
        );
        for tag in ["0.1.30", "v01.1.0", "v1.0.0-beta.1", "v1.0.0+local", "v1.0"] {
            assert!(release(tag).version().is_err());
        }
        let mut valid = release("v0.1.1");
        assert!(valid.asset_url(ASSET_NAME).is_ok());
        assert!(valid.asset_url("other.zip").is_err());
        valid.assets[0].browser_download_url = "https://example.com/gprox.exe".into();
        assert!(valid.asset_url(ASSET_NAME).is_err());
        valid.prerelease = true;
        assert!(valid.version().is_err());
        assert!(Version::new(0, 1, 30) > Version::new(0, 1, 9));
    }

    #[test]
    fn checksums_require_exact_asset_and_valid_unique_digest() {
        let hash = "ab".repeat(32);
        assert_eq!(
            checksum_for(&format!("{hash}  {ASSET_NAME}\n"), ASSET_NAME).unwrap(),
            hash
        );
        assert!(checksum_for(&format!("{hash} other.zip"), ASSET_NAME).is_err());
        assert!(checksum_for(&format!("abc {ASSET_NAME}"), ASSET_NAME).is_err());
        assert!(
            checksum_for(
                &format!("{hash} {ASSET_NAME}\n{hash} {ASSET_NAME}"),
                ASSET_NAME
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn github_uses_credentials_only_after_rate_limit_and_handles_no_release() {
        let calls = Arc::new(AtomicUsize::new(0));
        let app = Router::new()
            .route(
                "/repos/Ziiilk/gprox/releases/latest",
                get(
                    |State(calls): State<Arc<AtomicUsize>>, headers: HeaderMap| async move {
                        let call = calls.fetch_add(1, Ordering::SeqCst);
                        if call == 0 {
                            assert!(!headers.contains_key("authorization"));
                            StatusCode::FORBIDDEN
                        } else {
                            assert_eq!(headers["authorization"], "Bearer mock-token");
                            StatusCode::NOT_FOUND
                        }
                    },
                ),
            )
            .with_state(calls.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let github = Github {
            client: Client::builder().no_proxy().build().unwrap(),
            api_base: format!("http://{address}"),
        };
        let result = github
            .latest_with_token_source(|| async { Ok("mock-token".into()) })
            .await
            .unwrap();
        assert!(result.is_none());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        server.abort();
    }

    #[tokio::test]
    async fn downloaded_bytes_must_match_checksum() {
        let body = b"mock release archive";
        let app = Router::new().route("/archive", get(|| async { "mock release archive" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/archive", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive.zip");
        let client = Client::builder().no_proxy().build().unwrap();
        download_archive(&client, &url, &path, &format!("{:x}", Sha256::digest(body)))
            .await
            .unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), body);
        assert!(
            download_archive(&client, &url, &path, &"00".repeat(32))
                .await
                .is_err()
        );
        server.abort();
    }

    // Only a copied test executable enters this branch; the cargo test binary is
    // never replaced. The parent test exercises Windows' running-file semantics.
    #[cfg(windows)]
    #[test]
    fn replacement_child() {
        let Ok(candidate) = std::env::var("GPROX_TEST_REPLACEMENT") else {
            return;
        };
        self_update::self_replace::self_replace(candidate).unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn verifies_candidate_and_replaces_a_running_executable() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("candidate.rs");
        let candidate = directory.path().join("candidate.exe");
        std::fs::write(&source, "fn main() { println!(\"gprox 0.1.1\"); }").unwrap();
        let compiled = Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&candidate)
            .output()
            .await
            .unwrap();
        assert!(
            compiled.status.success(),
            "{}",
            String::from_utf8_lossy(&compiled.stderr)
        );
        verify_binary(&candidate, &Version::new(0, 1, 1))
            .await
            .unwrap();
        assert!(
            verify_binary(&candidate, &Version::new(0, 1, 2))
                .await
                .is_err()
        );
        let updater = directory.path().join("updater.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &updater).unwrap();
        let mut child = Command::new(&updater)
            .args(["--exact", "update::tests::replacement_child"])
            .env("GPROX_TEST_REPLACEMENT", &candidate)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let status = tokio::time::timeout(Duration::from_secs(15), child.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(status.success());
        verify_binary(&updater, &Version::new(0, 1, 1))
            .await
            .unwrap();
    }
}
