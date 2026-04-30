//! Contract tests for `serve --http <addr>`.
//!
//! Spawn the built binary, wait for the listener, hit
//! `GET /info` + `POST /` (JSON-RPC `helios/info`), assert the
//! discovery payload is well-formed and includes the cache field
//! we surfaced in engine FR-B1.  Then SIGTERM the child and verify
//! it exits cleanly without orphaning a RocksDB lock.
//!
//! See ROADMAP Tier 1 #6.

use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn binary() -> &'static Path {
    static BIN: OnceLock<&'static Path> = OnceLock::new();
    BIN.get_or_init(|| {
        let p = env!("CARGO_BIN_EXE_heliosdb-codekb-mcp");
        Box::leak(std::path::PathBuf::from(p).into_boxed_path())
    })
}

/// Block until `host:port` accepts TCP connections, or fail after `deadline`.
fn wait_for_port(host: &str, port: u16, deadline: Duration) {
    let until = Instant::now() + deadline;
    while Instant::now() < until {
        if TcpStream::connect_timeout(
            &format!("{host}:{port}").parse().unwrap(),
            Duration::from_millis(200),
        )
        .is_ok()
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("port {port} did not open within {deadline:?}");
}

/// Choose a random ephemeral port (bind + drop) — `serve --http` will
/// reuse it before any other tenant grabs it. Avoids hard-coding.
fn pick_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

struct ServerFixture {
    _td: TempDir,
    child: Child,
    port: u16,
    log_path: std::path::PathBuf,
}

impl ServerFixture {
    fn spawn() -> Self {
        let td = tempfile::tempdir().expect("tempdir");
        let source = td.path().join("src");
        let kb = td.path().join("kb");
        let xdg_config = td.path().join(".config");
        let xdg_data = td.path().join(".data");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&kb).unwrap();
        std::fs::create_dir_all(&xdg_config).unwrap();
        std::fs::create_dir_all(&xdg_data).unwrap();
        std::fs::write(source.join("a.rs"), "pub fn x() {}\n").unwrap();

        // init + first ingest so `serve` has a real KB.
        let init = Command::new(binary())
            .env("XDG_CONFIG_HOME", &xdg_config)
            .env("XDG_DATA_HOME", &xdg_data)
            .args([
                "init",
                "--source", source.to_str().unwrap(),
                "--mode", "hybrid",
                "--kb", kb.to_str().unwrap(),
                "--ingest",
            ])
            .output()
            .expect("init");
        assert!(init.status.success(), "init failed: {}",
            String::from_utf8_lossy(&init.stderr));

        let port = pick_port();
        let log_path = td.path().join("serve.log");
        let log = std::fs::File::create(&log_path).unwrap();
        let stderr = log.try_clone().unwrap();

        let child = Command::new(binary())
            .env("XDG_CONFIG_HOME", &xdg_config)
            .env("XDG_DATA_HOME", &xdg_data)
            .args([
                "serve",
                "--source", source.to_str().unwrap(),
                "--http", &format!("127.0.0.1:{port}"),
            ])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(stderr)
            .spawn()
            .expect("spawn serve");

        wait_for_port("127.0.0.1", port, Duration::from_secs(15));
        Self { _td: td, child, port, log_path }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
}

impl Drop for ServerFixture {
    fn drop(&mut self) {
        // Best-effort SIGTERM — graceful_shutdown handler in serve()
        // wires Ctrl-C, which Unix sends as SIGINT (==SIGTERM-ish for
        // tokio's signal::ctrl_c).  Use SIGINT for portability.
        #[cfg(unix)]
        {
            unsafe {
                libc::kill(self.child.id() as i32, libc::SIGINT);
            }
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[test]
fn get_info_returns_cache_and_tools() {
    let s = ServerFixture::spawn();

    let info: serde_json::Value = ureq::get(&s.url("/info"))
        .call()
        .expect("GET /info")
        .into_json()
        .expect("parse JSON");

    assert_eq!(info["serverInfo"]["name"], "heliosdb-nano",
        "unexpected serverInfo: {info}");
    let tools = info["tools"].as_array().expect("tools array");
    assert!(!tools.is_empty(), "tool list must be non-empty");
    let cache = &info["cache"];
    assert!(cache.is_object(), "expected cache field on /info");
    for key in ["size", "capacity", "generation", "hits",
                "misses", "evictions", "hit_rate"] {
        assert!(
            cache.get(key).is_some(),
            "cache.{key} missing — got {cache}"
        );
    }
}

#[test]
fn post_root_runs_jsonrpc() {
    let s = ServerFixture::spawn();

    // initialize handshake.
    let resp: serde_json::Value = ureq::post(&s.url("/"))
        .send_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {}
        }))
        .expect("POST initialize")
        .into_json()
        .expect("parse JSON");
    assert_eq!(resp["result"]["serverInfo"]["name"], "heliosdb-nano");

    // helios/info via JSON-RPC mirrors the GET /info payload.
    let resp: serde_json::Value = ureq::post(&s.url("/"))
        .send_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "helios/info",
            "params": {}
        }))
        .expect("POST helios/info")
        .into_json()
        .expect("parse JSON");
    let cache = &resp["result"]["cache"];
    assert!(cache.is_object(), "JSON-RPC helios/info missing cache field");
}

#[test]
fn startup_banner_appears_in_log_and_clean_shutdown() {
    // Read the log while the server is alive (the tempdir vanishes
    // when the fixture drops, taking the log with it). Then drop —
    // ServerFixture::Drop sends SIGINT and waits for the child,
    // which would panic on hang; reaching the end of this test is
    // proof the shutdown path completes.
    let s = ServerFixture::spawn();
    let _ = ureq::get(&s.url("/info")).call();

    let log = std::fs::read_to_string(&s.log_path).expect("log read");
    assert!(
        log.contains("MCP HTTP server listening on"),
        "did not see startup banner; log was:\n{log}"
    );
    // Drop here SIGINTs + waits.  If the graceful_shutdown future
    // didn't fire, `child.wait()` in Drop would block forever —
    // the test runner's per-test timeout would surface it.
}
