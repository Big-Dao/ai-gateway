//! Shared test helpers for gateway-server integration tests.
//!
//! The MVP 0 suite spawns a **real** `gateway-server` binary for each test by
//! pointing `tokio::process::Command` at the path that cargo's
//! `CARGO_BIN_EXE_gateway-server` env-var exposes at compile time. cargo
//! populates that variable automatically for *cargo test* runs, so these
//! targets "just work" when invoked via `cargo test`. Run the suite with:
//!
//!   cargo build --bin gateway-server && cargo test --package gateway-server
//!
//! Each [`TestServer`] picks an unused ephemeral port (via
//! `TcpListener::bind("127.0.0.1:0")`) so parallel tests never collide.

// Test helpers: not every spawn variant is exercised by every test file.
#![allow(dead_code)]

use std::net::TcpListener;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};

pub struct TestServer {
    pub base_url: String,
    child: Child,
}

/// Pick an OS-assigned free port and return it. Releases the binding
/// immediately so the spawned server can take it.
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

impl TestServer {
    /// Spawn a gateway-server process. A TOML config file is written to
    /// `/tmp/gateway-test-{port}.toml` and the child's stdout/stderr are
    /// tee'd to `/tmp/gateway-test-{port}.log`. That log path is dumped to
    /// stderr by `panic!` on startup failure so CI runners can grep it.
    ///
    /// `extra_envs` is a list of `(AI_GATEWAY__-prefixed-key, JSON-string)`
    /// pairs forwarded verbatim as env vars (e.g., `RATE_LIMIT__RPM=5`).
    pub async fn spawn_with(api_keys: &[&str], extra_envs: &[(&str, String)]) -> Self {
        let port = free_port();
        let keys_json: String = serde_json::to_string(&api_keys).expect("serialize api_keys");

        // Write a minimal config with a stub ollama provider. The stub server
        // is never contacted — every MVP 0 test hits routes that don't require
        // a live upstream (health checks + "auth fails before model dispatch"
        // paths). `[server].host` is left to the default 0.0.0.0 so we listen
        // on all interfaces, which is what reqwest's `127.0.0.1` targets.
        let config_path = std::env::temp_dir().join(format!("gateway-test-{}.toml", port));
        let log_path = std::env::temp_dir().join(format!("gateway-test-{}.log", port));
        let toml = format!(
            r#"[server]
host = "0.0.0.0"
port = {port}

[auth]
api_keys = {keys_json}

[providers.ollama]
models = ["smoke-model"]
base_url = "http://127.0.0.1:11111"
"#,
        );
        tokio::fs::write(&config_path, toml)
            .await
            .expect("write test config");

        let bin_path = env!("CARGO_BIN_EXE_gateway-server");
        // We can't `try_clone()` an axum::Stdio from OpenOptions; open the
        // log file twice so we get fresh FDs for each pipe.
        let out_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("open log file");
        let err_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("open log file");
        let out_stdio = Stdio::from(out_file);
        let err_stdio = Stdio::from(err_file);

        let mut cmd = Command::new(bin_path);
        cmd.env("CONFIG_PATH", config_path.to_str().unwrap())
            .env("RUST_LOG", "info,gateway_server=debug")
            .env("AI_GATEWAY__AUTH__ENABLED", "true")
            .stdout(out_stdio)
            .stderr(err_stdio);

        // Determine the port we should actually poll. If the caller passed an
        // env override for SERVER__PORT, that wins over the TOML value (and
        // the earlier free_port() pick).
        let mut effective_port = port;
        for (key, val) in extra_envs {
            if *key == "SERVER__PORT" {
                if let Ok(p) = val.parse::<u16>() {
                    effective_port = p;
                }
            }
            cmd.env(format!("AI_GATEWAY__{key}"), val);
        }

        let mut child = cmd.spawn().expect("spawn gateway-server binary");
        let base_url = format!("http://127.0.0.1:{effective_port}");

        let client = reqwest::Client::new();
        let mut started = false;
        let mut last_err = String::new();
        for _ in 0..100 {
            match client.get(format!("{base_url}/healthz")).send().await {
                Ok(resp) if resp.status().is_success() => {
                    started = true;
                    break;
                }
                Ok(resp) => {
                    last_err = format!("Got non-200 status {}", resp.status());
                }
                Err(e) => {
                    last_err = e.to_string();
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if !started {
            let _ = child.start_kill();
            let log_tail = tokio::fs::read_to_string(&log_path)
                .await
                .unwrap_or_else(|_| "(could not read log)".into());
            panic!(
                "gateway-server did not become healthy within ~20s. \
                 base_url={base_url}\n\
                 config={config_path:?}\n\
                 last_err={last_err}\n\
                 --- LAST 2000 BYTES OF LOG ({log_path:?}) ---\n{}",
                log_tail
                    .chars()
                    .rev()
                    .take(2000)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            );
        }

        // Best-effort cleanup of the temporary config; the log stays on disk
        // for post-mortem debugging if a later test fails.
        let _ = tokio::fs::remove_file(&config_path).await;

        Self { base_url, child }
    }

    pub async fn spawn(api_keys: &[&str]) -> Self {
        Self::spawn_with(api_keys, &[]).await
    }

    /// Spawn with explicit (plaintext_key, tenant_id, role) tuples.
    /// Requires the gateway-server binary to support
    /// `auth.structured_keys` config (added in MVP 1).
    pub async fn spawn_with_keys(keys: &[(&str, &str, &str)]) -> Self {
        let port = free_port();
        let config_path = std::env::temp_dir().join(format!("gateway-test-{}.toml", port));
        let log_path = std::env::temp_dir().join(format!("gateway-test-{}.log", port));

        // Format structured_keys as an array of [key, tenant, role] for TOML
        let entries: Vec<String> = keys
            .iter()
            .map(|(k, t, r)| {
                format!(
                    "[\"{}\", \"{}\", \"{}\"]",
                    k.replace('"', "\\\""),
                    t.replace('"', "\\\""),
                    r.replace('"', "\\\"")
                )
            })
            .collect();
        let structured = format!("[{}]", entries.join(", "));

        let toml = format!(
            r#"[server]
host = "0.0.0.0"
port = {port}

[auth]
structured_keys = {structured}

[providers.ollama]
models = ["smoke-model"]
base_url = "http://127.0.0.1:11111"
"#,
        );
        tokio::fs::write(&config_path, toml)
            .await
            .expect("write config");

        let bin_path = env!("CARGO_BIN_EXE_gateway-server");
        let out_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .expect("open log file");
        let out_stdio = Stdio::from(out_file);
        let err_stdio = Stdio::from(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .expect("open log file"),
        );

        let mut cmd = Command::new(bin_path);
        cmd.env("CONFIG_PATH", config_path.to_str().unwrap())
            .env("RUST_LOG", "info,gateway_server=debug")
            .stdout(out_stdio)
            .stderr(err_stdio);

        let base_url = format!("http://127.0.0.1:{port}");
        let client = reqwest::Client::new();
        let mut started = false;
        let mut last_err = String::new();
        let mut child = cmd.spawn().expect("spawn gateway-server binary");
        for _ in 0..100 {
            match client.get(format!("{base_url}/healthz")).send().await {
                Ok(resp) if resp.status().is_success() => {
                    started = true;
                    break;
                }
                Ok(resp) => last_err = format!("Got {}", resp.status()),
                Err(e) => last_err = e.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if !started {
            let _ = child.start_kill();
            panic!("gateway-server not healthy: {last_err}");
        }

        let _ = tokio::fs::remove_file(&config_path).await;

        Self { base_url, child }
    }

    pub async fn spawn_with_rpm(api_keys: &[&str], rpm: u16) -> Self {
        Self::spawn_with(
            api_keys,
            &[(
                "RATE_LIMIT__REQUESTS_PER_MINUTE",
                serde_json::to_string(&rpm).unwrap(),
            )],
        )
        .await
    }

    /// Spawn with structured (tenant-scoped) keys AND custom rate-limit RPM.
    /// Needed for quota tests where we want rate-limit to never trip before quota.
    pub async fn spawn_with_keys_and_rpm(keys: &[(&str, &str, &str)], rpm: u16) -> Self {
        let port = free_port();
        let config_path = std::env::temp_dir().join(format!("gateway-test-{}.toml", port));
        let log_path = std::env::temp_dir().join(format!("gateway-test-{}.log", port));

        let entries: Vec<String> = keys
            .iter()
            .map(|(k, t, r)| {
                format!(
                    "[\"{}\", \"{}\", \"{}\"]",
                    k.replace('"', "\\\""),
                    t.replace('"', "\\\""),
                    r.replace('"', "\\\"")
                )
            })
            .collect();
        let structured = format!("[{}]", entries.join(", "));

        let toml = format!(
            r#"[server]
host = "0.0.0.0"
port = {port}

[auth]
structured_keys = {structured}

[providers.ollama]
models = ["smoke-model"]
base_url = "http://127.0.0.1:11111"
"#,
        );
        tokio::fs::write(&config_path, toml)
            .await
            .expect("write config");

        let bin_path = env!("CARGO_BIN_EXE_gateway-server");
        let out_stdio = Stdio::from(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .expect("open log file"),
        );
        let err_stdio = Stdio::from(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&log_path)
                .expect("open log file"),
        );

        let mut cmd = Command::new(bin_path);
        cmd.env("CONFIG_PATH", config_path.to_str().unwrap())
            .env("RUST_LOG", "info,gateway_server=debug")
            .env(
                "AI_GATEWAY__RATE_LIMIT__REQUESTS_PER_MINUTE",
                rpm.to_string(),
            )
            .stdout(out_stdio)
            .stderr(err_stdio);

        let base_url = format!("http://127.0.0.1:{port}");
        let mut child = cmd.spawn().expect("spawn gateway-server binary");
        let client = reqwest::Client::new();
        let mut started = false;
        for _ in 0..100 {
            match client.get(format!("{base_url}/healthz")).send().await {
                Ok(resp) if resp.status().is_success() => {
                    started = true;
                    break;
                }
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if !started {
            let _ = child.start_kill();
            panic!("gateway-server not healthy");
        }

        let _ = tokio::fs::remove_file(&config_path).await;

        Self { base_url, child }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

// Silence dead-code warnings for helper methods we keep for ergonomics.
#[allow(dead_code)]
impl TestServer {
    pub async fn get(&self, path: &str) -> reqwest::Response {
        reqwest::Client::new()
            .get(format!("{}{path}", self.base_url))
            .send()
            .await
            .expect("GET request failed")
    }

    pub async fn get_with(&self, path: &str, api_key: Option<&str>) -> reqwest::Response {
        let mut req = reqwest::Client::new().get(format!("{}{path}", self.base_url));
        if let Some(k) = api_key {
            req = req.bearer_auth(k);
        }
        req.send().await.expect("GET request failed")
    }
}
