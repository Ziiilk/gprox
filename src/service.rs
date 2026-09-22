use crate::{
    StartArgs,
    api::{self, AppState},
    backend::{Backend, BackendConfig},
    config::{self, Settings},
    process,
};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Request, State},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    fs,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{net::TcpListener, sync::Semaphore};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

#[derive(Clone, Serialize, Deserialize)]
struct Runtime {
    pid: u32,
    base_url: String,
    admin_port: u16,
    admin_token: String,
    instance: String,
}

struct RuntimeFile(PathBuf);
impl Drop for RuntimeFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[derive(Clone)]
struct Control {
    runtime: Runtime,
    shutdown: CancellationToken,
}

fn control_router(control: Control) -> Router {
    Router::new()
        .route(
            "/status",
            get(|State(control): State<Control>| async move {
                Json(json!({"instance":control.runtime.instance,"pid":control.runtime.pid}))
            }),
        )
        .route(
            "/stop",
            post(|State(control): State<Control>| async move {
                control.shutdown.cancel();
                Json(json!({"stopping":true}))
            }),
        )
        .layer(middleware::from_fn_with_state(
            control.clone(),
            control_auth,
        ))
        .with_state(control)
}

async fn control_auth(State(control): State<Control>, request: Request, next: Next) -> Response {
    if !api::valid_bearer(request.headers(), &control.runtime.admin_token) {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            "Invalid management token",
        )
            .into_response();
    }
    next.run(request).await
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(2))
        .build()?)
}

fn read_runtime(home: &Path) -> Option<Runtime> {
    serde_json::from_slice(&fs::read(home.join("runtime.json")).ok()?).ok()
}

async fn is_running(runtime: &Runtime) -> bool {
    let Ok(client) = client() else {
        return false;
    };
    let result = client
        .get(format!("http://127.0.0.1:{}/status", runtime.admin_port))
        .bearer_auth(&runtime.admin_token)
        .send()
        .await;
    if let Ok(response) = result {
        if !response.status().is_success() {
            return false;
        }
        if let Ok(value) = response.json::<serde_json::Value>().await {
            return value["instance"] == runtime.instance;
        }
    }
    false
}

fn prepared_settings(home: &Path, args: StartArgs) -> Result<Settings> {
    let _lock = config::lock(home, "settings.lock")?;
    let mut settings = Settings::load(home)?;
    settings.apply(args)?;
    settings.codex = Some(process::resolve_codex(settings.codex.as_deref())?);
    settings.save(home)?;
    Ok(settings)
}

pub async fn run(home: PathBuf, args: StartArgs) -> Result<()> {
    let _run_lock = config::lock(&home, "run.lock")?;
    let settings = prepared_settings(&home, args)?;
    let workspace = home.join("workspace");
    fs::create_dir_all(&workspace)?;
    let backend_config = BackendConfig {
        executable: settings.codex.clone().unwrap(),
        workspace,
    };
    let models = tokio::time::timeout(Duration::from_secs(45), async {
        let mut backend = Backend::connect(&backend_config).await?;
        backend.models().await
    })
    .await
    .context("Codex initialization timed out")??;
    let listener = TcpListener::bind(SocketAddr::new(settings.host, settings.port))
        .await
        .context("Cannot bind proxy address (port may be in use)")?;
    let admin = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let display_host = match settings.host {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        host => host,
    };
    let runtime = Runtime {
        pid: std::process::id(),
        base_url: format!("http://{}/v1", SocketAddr::new(display_host, settings.port)),
        admin_port: admin.local_addr()?.port(),
        admin_token: config::token(),
        instance: config::token(),
    };
    let shutdown = CancellationToken::new();
    let tasks = TaskTracker::new();
    let state = Arc::new(AppState {
        api_key: settings.api_key,
        backend: backend_config,
        timeout: Duration::from_secs(settings.timeout),
        slots: Arc::new(Semaphore::new(settings.max_concurrency)),
        shutdown: shutdown.clone(),
        tasks: tasks.clone(),
        models,
    });
    let public_app = api::router(state);
    let admin_app = control_router(Control {
        runtime: runtime.clone(),
        shutdown: shutdown.clone(),
    });
    let stop_signal = shutdown.clone();
    let signal_task = tokio::spawn(async move {
        shutdown_signal().await;
        stop_signal.cancel();
    });
    let public_shutdown = shutdown.clone();
    let mut public_task = tokio::spawn(async move {
        axum::serve(listener, public_app)
            .with_graceful_shutdown(public_shutdown.cancelled_owned())
            .await
    });
    let admin_shutdown = shutdown.clone();
    let mut admin_task = tokio::spawn(async move {
        axum::serve(admin, admin_app)
            .with_graceful_shutdown(admin_shutdown.cancelled_owned())
            .await
    });
    config::write_json(&home.join("runtime.json"), &runtime)?;
    let _runtime_file = RuntimeFile(home.join("runtime.json"));
    println!("gprox listening on {}", runtime.base_url);
    println!(
        "API key: run `gprox key` (credentials: {})",
        home.join("config.json").display()
    );
    println!("Stop: Ctrl+C or gprox stop");
    let server_error = tokio::select! {
        _ = shutdown.cancelled() => None,
        result = &mut public_task => Some(format!("Public listener exited: {result:?}")),
        result = &mut admin_task => Some(format!("Management listener exited: {result:?}")),
    };
    shutdown.cancel();
    signal_task.abort();
    tasks.close();
    let _ = tokio::time::timeout(Duration::from_secs(3), tasks.wait()).await;
    // Do not let an idle/slow HTTP peer keep the service alive indefinitely.
    tokio::time::sleep(Duration::from_millis(100)).await;
    public_task.abort();
    admin_task.abort();
    println!("gprox stopped");
    if let Some(error) = server_error {
        bail!(error);
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

pub async fn start_background(home: &Path, args: StartArgs) -> Result<()> {
    let _launch_lock = config::lock(home, "launch.lock")?;
    if let Some(runtime) = read_runtime(home)
        && is_running(&runtime).await
    {
        println!(
            "gprox already running: {} (PID {})",
            runtime.base_url, runtime.pid
        );
        return Ok(());
    }
    {
        let _run_lock = config::lock(home, "run.lock")?;
        prepared_settings(home, args)?;
    }
    let log_path = home.join("service.log");
    let log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("--home")
        .arg(home)
        .arg("start")
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Windows otherwise inherits the launcher's captured output handles too,
        // so shells/Command::output wait for the daemon before observing EOF.
        process::detach_standard_handles()?;
        command.creation_flags(0x08000000 | 0x00000200); // no window + independent process group
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // setsid is async-signal-safe and detaches from the controlling terminal.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
    let mut child = command.spawn().context("Cannot start background gprox")?;
    for _ in 0..240 {
        if let Some(status) = child.try_wait()? {
            bail!(
                "Background gprox exited ({status}). See {}",
                log_path.display()
            );
        }
        if let Some(runtime) = read_runtime(home)
            && runtime.pid == child.id()
            && is_running(&runtime).await
        {
            println!(
                "gprox running in background: {} (PID {})",
                runtime.base_url, runtime.pid
            );
            println!("Key: gprox key\nLog: {}", log_path.display());
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let _ = child.kill();
    let _ = child.wait();
    bail!("Background startup timed out. See {}", log_path.display());
}

pub async fn stop(home: &Path) -> Result<()> {
    let Some(runtime) = read_runtime(home) else {
        if config::lock(home, "run.lock").is_err() {
            bail!("gprox is initializing; retry stop shortly");
        }
        println!("gprox is stopped");
        return Ok(());
    };
    if !is_running(&runtime).await {
        if config::lock(home, "run.lock").is_err() {
            bail!(
                "gprox holds its lock but management endpoint is unavailable; no process was force-killed"
            );
        }
        println!("gprox is stopped (stale runtime state)");
        return Ok(());
    }
    client()?
        .post(format!("http://127.0.0.1:{}/stop", runtime.admin_port))
        .bearer_auth(runtime.admin_token)
        .send()
        .await?
        .error_for_status()?;
    for _ in 0..100 {
        if config::lock(home, "run.lock").is_ok() {
            println!("gprox stopped");
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("Stop requested, but gprox has not released its lock yet");
}

pub async fn status(home: &Path) -> Result<()> {
    if let Some(runtime) = read_runtime(home)
        && is_running(&runtime).await
    {
        println!(
            "running\nPID: {}\nBase URL: {}\nState: {}",
            runtime.pid,
            runtime.base_url,
            home.display()
        );
        return Ok(());
    }
    if config::lock(home, "run.lock").is_err() {
        println!("initializing or management endpoint unavailable");
    } else {
        println!("stopped");
    }
    Ok(())
}
