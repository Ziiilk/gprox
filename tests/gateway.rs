use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
    time::Duration,
};

const BIN: &str = env!("CARGO_BIN_EXE_gprox");
fn cli(home: &Path, args: &[&str]) -> Output {
    Command::new(BIN)
        .env("GPROX_MOCK_PID_FILE", home.join("mock-pids.log"))
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}
fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}
struct Cleanup(std::path::PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = cli(&self.0, &["stop"]);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn lifecycle_auth_streaming_errors_and_cancellation() {
    let dir = tempfile::tempdir().unwrap();
    let mock = dir.path().join(if cfg!(windows) {
        "mock-codex.exe"
    } else {
        "mock-codex"
    });
    let compile = Command::new("rustc")
        .args(["--edition=2024", "tests/fixtures/mock_codex.rs", "-o"])
        .arg(&mock)
        .output()
        .unwrap();
    success(compile);
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port().to_string();
    drop(socket);
    success(cli(
        dir.path(),
        &[
            "service",
            "start",
            "--codex",
            mock.to_str().unwrap(),
            "--port",
            &port,
            "--timeout",
            "2",
            "--max-concurrency",
            "1",
        ],
    ));
    let _cleanup = Cleanup(dir.path().to_owned());
    let first: Value =
        serde_json::from_slice(&std::fs::read(dir.path().join("runtime.json")).unwrap()).unwrap();
    assert!(success(cli(dir.path(), &["service", "start"])).contains("already running"));
    assert!(success(cli(dir.path(), &["status"])).starts_with("running"));
    assert!(!cli(dir.path(), &["start"]).status.success());
    let key = success(cli(dir.path(), &["key"])).trim().to_owned();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    assert_eq!(
        client
            .get(format!("{base}/v1/models"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/v1/models"))
            .bearer_auth("wrong")
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let models: Value = client
        .get(format!("{base}/v1/models"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["data"][0]["id"], "mock-model");
    // API credentials cannot control the loopback-only management listener.
    let admin = format!("http://127.0.0.1:{}/stop", first["admin_port"]);
    assert_eq!(
        client
            .post(admin)
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let chat = json!({"model":"mock-model","messages":[{"role":"user","content":"hello"}]});
    let result: Value = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&key)
        .json(&chat)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(result["choices"][0]["message"]["content"], "Hello 世界");
    assert_eq!(result["usage"]["total_tokens"], 13);
    let mut stream = chat.clone();
    stream["stream"] = json!(true);
    stream["stream_options"] = json!({"include_usage":true});
    let result = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&key)
        .json(&stream)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(result.contains("[DONE]"));
    assert!(result.contains("世界"));
    assert!(result.contains("\"total_tokens\":13"));
    for streaming in [false, true] {
        let result = client
            .post(format!("{base}/v1/responses"))
            .bearer_auth(&key)
            .json(&json!({"model":"mock-model","input":"hi","stream":streaming}))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        if streaming {
            assert!(result.contains("event: response.completed"));
            assert!(result.contains("event: response.output_text.delta"));
        } else {
            let response: Value = serde_json::from_str(&result).unwrap();
            assert_eq!(response["output"][0]["content"][0]["text"], "Hello 世界");
        }
    }
    let mut invalid = chat.clone();
    invalid["tools"] = json!([]);
    assert_eq!(
        client
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(&key)
            .json(&invalid)
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let mut failed = chat.clone();
    failed["messages"][0]["content"] = json!("MOCK_ERROR");
    assert_eq!(
        client
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(&key)
            .json(&failed)
            .send()
            .await
            .unwrap()
            .status(),
        502
    );
    let mut hanging = chat.clone();
    hanging["messages"][0]["content"] = json!("MOCK_TIMEOUT");
    let request = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&key)
        .json(&hanging);
    let task = tokio::spawn(async move { request.send().await.unwrap() });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        client
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(&key)
            .json(&chat)
            .send()
            .await
            .unwrap()
            .status(),
        429
    );
    assert_eq!(task.await.unwrap().status(), 504);
    // Dropping an SSE connection must release its concurrency slot immediately.
    hanging["stream"] = json!(true);
    let response = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&key)
        .json(&hanging)
        .send()
        .await
        .unwrap();
    drop(response);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        client
            .post(format!("{base}/v1/chat/completions"))
            .bearer_auth(&key)
            .json(&chat)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let active = client
        .post(format!("{base}/v1/chat/completions"))
        .bearer_auth(&key)
        .json(&hanging)
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    success(cli(dir.path(), &["service", "stop"]));
    drop(active);
    assert!(success(cli(dir.path(), &["status"])).starts_with("stopped"));
    assert!(!dir.path().join("runtime.json").exists());
    // The same stop command also terminates a foreground start.
    let mut foreground = Command::new(BIN)
        .arg("--home")
        .arg(dir.path())
        .arg("start")
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..100 {
        if dir.path().join("runtime.json").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    success(cli(dir.path(), &["stop"]));
    assert!(foreground.wait().unwrap().success());
    success(cli(dir.path(), &["stop"]));
    #[cfg(windows)]
    for pid in std::fs::read_to_string(dir.path().join("mock-pids.log"))
        .unwrap()
        .lines()
    {
        use windows_sys::Win32::{
            Foundation::{CloseHandle, WAIT_TIMEOUT},
            System::Threading::{OpenProcess, WaitForSingleObject},
        };
        unsafe {
            let handle = OpenProcess(0x00100000, 0, pid.parse().unwrap()); // SYNCHRONIZE
            if !handle.is_null() {
                let result = WaitForSingleObject(handle, 2000);
                CloseHandle(handle);
                assert_ne!(result, WAIT_TIMEOUT, "Backend PID {pid} survived shutdown");
            }
        }
    }
}
