// Compiled separately by gateway.rs. No network or real subscription is used.
use std::io::{self, BufRead, Write};
fn emit(value: &str) { println!("{value}"); io::stdout().flush().unwrap(); }
fn main() {
    if let Some(path) = std::env::var_os("GPROX_MOCK_PID_FILE") {
        let mut file = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
        writeln!(file, "{}", std::process::id()).unwrap();
    }
    for line in io::stdin().lock().lines() {
        let line = line.unwrap();
        let id = line.split("\"id\":").nth(1).and_then(|s|s.split([',','}']).next()).unwrap_or("0");
        let result = if line.contains("\"method\":\"initialize\"") { Some("{}") }
        else if line.contains("\"method\":\"account/read\"") { Some(r#"{"account":{"type":"chatgpt"}}"#) }
        else if line.contains("\"method\":\"model/list\"") { Some(r#"{"data":[{"model":"mock-model"}],"nextCursor":null}"#) }
        else if line.contains("\"method\":\"config/read\"") { Some(r#"{"config":{"mcp_servers":{"unsafe":{"command":"mock-command"}}}}"#) }
        else if line.contains("\"method\":\"thread/start\"") {
            assert!(line.contains("mcp_servers.unsafe.enabled\":false"));
            assert!(line.contains("\"ephemeral\":true"));
            Some(r#"{"thread":{"id":"thread-test"}}"#)
        } else if line.contains("\"method\":\"turn/start\"") {
            emit(&format!(r#"{{"id":{id},"result":{{}}}}"#));
            if line.contains("MOCK_TIMEOUT") { std::thread::sleep(std::time::Duration::from_secs(30)); continue; }
            if line.contains("MOCK_ERROR") {
                emit(r#"{"method":"turn/completed","params":{"turn":{"status":"failed","error":{"message":"mock upstream failure"}}}}"#);
                continue;
            }
            emit(r#"{"method":"item/agentMessage/delta","params":{"itemId":"item-test","delta":"Hello "}}"#);
            emit(r#"{"method":"item/agentMessage/delta","params":{"itemId":"item-test","delta":"世界"}}"#);
            emit(r#"{"method":"item/completed","params":{"item":{"id":"item-test","type":"agentMessage","text":"Hello 世界"}}}"#);
            emit(r#"{"method":"thread/tokenUsage/updated","params":{"tokenUsage":{"total":{"inputTokens":10,"outputTokens":3,"totalTokens":13,"cachedInputTokens":2,"reasoningOutputTokens":1}}}}"#);
            emit(r#"{"method":"turn/completed","params":{"turn":{"status":"completed"}}}"#);
            None
        } else { None };
        if let Some(result) = result { emit(&format!(r#"{{"id":{id},"result":{result}}}"#)); }
    }
}
