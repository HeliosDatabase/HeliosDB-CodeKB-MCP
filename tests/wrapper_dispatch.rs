//! Layer 2 contract test — plugin wrapper tools end-to-end through
//! the HTTP gateway.
//!
//! Spawns the binary under `--profile standard`, verifies:
//! * Every plugin wrapper appears in `tools/list`.
//! * `tools/call helios_repo_summary` returns the `cards_not_built`
//!   shape on a tempdir KB that hasn't been distill-ingested.
//! * `tools/call helios_symbol_card` returns `not_found` for a missing
//!   symbol on a freshly-ingested fixture KB (no panic on empty
//!   code-graph tables).
//! * `tools/call helios_outline_first` returns a valid (possibly
//!   empty) `sections` array — no Rust panic on a doc-light fixture.

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

fn pick_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

struct Fixture {
    _td: TempDir,
    child: Child,
    port: u16,
}

impl Fixture {
    fn spawn() -> Self {
        let td = tempfile::tempdir().unwrap();
        let source = td.path().join("src");
        let kb = td.path().join("kb");
        let xdg_config = td.path().join(".config");
        let xdg_data = td.path().join(".data");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::create_dir_all(&kb).unwrap();
        std::fs::create_dir_all(&xdg_config).unwrap();
        std::fs::create_dir_all(&xdg_data).unwrap();
        // One source file so init+ingest doesn't no-op the schema.
        std::fs::write(
            source.join("a.rs"),
            "/// adds two numbers\npub fn add(a: i32, b: i32) -> i32 { a + b }\n",
        )
        .unwrap();
        std::fs::write(source.join("README.md"), "# Sample\n\nA tiny crate.\n").unwrap();

        let init = Command::new(binary())
            .env("XDG_CONFIG_HOME", &xdg_config)
            .env("XDG_DATA_HOME", &xdg_data)
            .args([
                "init",
                "--source",
                source.to_str().unwrap(),
                "--mode",
                "hybrid",
                "--kb",
                kb.to_str().unwrap(),
                "--ingest",
            ])
            .output()
            .expect("init");
        assert!(
            init.status.success(),
            "init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );

        let port = pick_port();
        let log = std::fs::File::create(td.path().join("serve.log")).unwrap();
        let stderr = log.try_clone().unwrap();

        let child = Command::new(binary())
            .env("XDG_CONFIG_HOME", &xdg_config)
            .env("XDG_DATA_HOME", &xdg_data)
            .args([
                "serve",
                "--source",
                source.to_str().unwrap(),
                "--http",
                &format!("127.0.0.1:{port}"),
                "--profile",
                "standard",
                "--strip-tool-descriptions",
                "none",
            ])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(stderr)
            .spawn()
            .expect("spawn serve");
        wait_for_port("127.0.0.1", port, Duration::from_secs(15));
        Self {
            _td: td,
            child,
            port,
        }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }

    fn call(&self, name: &str, args: serde_json::Value) -> serde_json::Value {
        ureq::post(&self.url())
            .send_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/call",
                "params": { "name": name, "arguments": args }
            }))
            .expect("POST tools/call")
            .into_json()
            .expect("parse JSON")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as i32, libc::SIGINT);
        }
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn all_plugin_wrappers_appear_in_tools_list() {
    let f = Fixture::spawn();
    let resp: serde_json::Value = ureq::post(&f.url())
        .send_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list",
            "params": {}
        }))
        .expect("POST tools/list")
        .into_json()
        .expect("parse JSON");
    let names: Vec<&str> = resp["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().expect("name"))
        .collect();
    for w in [
        "helios_repo_summary",
        "helios_outline_first",
        "helios_doc_drill",
        "helios_semantic_filter",
        "helios_git_summary",
        "helios_symbol_card",
    ] {
        assert!(
            names.contains(&w),
            "plugin wrapper {w} missing from tools/list under --profile standard; got {names:?}"
        );
    }
}

#[test]
fn helios_repo_summary_returns_envelope() {
    let f = Fixture::spawn();
    let resp = f.call("helios_repo_summary", serde_json::json!({"detail": "minimal"}));
    // MCP `tools/call` envelope: {"result": {"content":[{"type":"text","text":"<json>"}], "isError": …}}
    let content_text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    let inner: serde_json::Value =
        serde_json::from_str(content_text).expect("inner JSON parses");
    // Either real card data, or the cards_not_built sentinel.
    assert!(
        inner.get("files").is_some() || inner["status"] == "cards_not_built",
        "unexpected helios_repo_summary shape: {inner}"
    );
}

#[test]
fn helios_symbol_card_handles_missing_symbol_gracefully() {
    let f = Fixture::spawn();
    let resp = f.call(
        "helios_symbol_card",
        serde_json::json!({"qualified_name": "totally_not_a_real_symbol"}),
    );
    let content_text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    let inner: serde_json::Value =
        serde_json::from_str(content_text).expect("inner JSON parses");
    // Missing symbol → not_found, never a panic / JSON-RPC -32xxx.
    assert_eq!(
        inner["status"], "not_found",
        "missing symbol should yield status=not_found; got {inner}"
    );
    assert!(
        resp.get("error").is_none(),
        "should NOT emit a JSON-RPC error frame for a not-found symbol"
    );
}

#[test]
fn helios_outline_first_returns_sections_array() {
    let f = Fixture::spawn();
    let resp = f.call("helios_outline_first", serde_json::json!({"query": "Sample"}));
    let content_text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content");
    let inner: serde_json::Value =
        serde_json::from_str(content_text).expect("inner JSON parses");
    let sections = inner["sections"].as_array().expect("sections array");
    // Empty allowed (depends on whether graph_rag projected the README
    // section); shape must be valid.
    let _ = sections.len();
}
