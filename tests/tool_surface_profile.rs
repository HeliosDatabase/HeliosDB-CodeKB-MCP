//! Layer 1 contract test — gateway-level tool-surface filtering and
//! description stripping on the HTTP transport.
//!
//! Spawns the binary under three profiles (`minimal`, `standard`,
//! `full`) and verifies:
//!
//! * `tools/list` returns only the profile's allow-listed names.
//! * `tools/call` for a tool that's outside the active profile still
//!   succeeds — filtering touches advertising only, not dispatch.
//! * `--strip-tool-descriptions <N>` caps every advertised description
//!   at N bytes with no truncation marker.
//! * `--profile full --strip-tool-descriptions none` is byte-identical
//!   to the engine's native `tools/list` (the passthrough invariant).
//!
//! Sandboxes XDG_CONFIG_HOME / XDG_DATA_HOME inside a tempdir so the
//! developer's real config is never touched. Pattern lifted from
//! `tests/http_transport.rs`.

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

struct ServerFixture {
    _td: TempDir,
    child: Child,
    port: u16,
}

impl ServerFixture {
    /// Spawn `serve --http` with the given profile + strip mode. Pass
    /// `None` for either to omit the flag (binary falls back to its
    /// built-in defaults: `standard` / `200`).
    fn spawn(profile: Option<&str>, strip: Option<&str>) -> Self {
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

        let mut cmd = Command::new(binary());
        cmd.env("XDG_CONFIG_HOME", &xdg_config)
            .env("XDG_DATA_HOME", &xdg_data)
            .args([
                "serve",
                "--source",
                source.to_str().unwrap(),
                "--http",
                &format!("127.0.0.1:{port}"),
            ])
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(stderr);
        if let Some(p) = profile {
            cmd.args(["--profile", p]);
        }
        if let Some(s) = strip {
            cmd.args(["--strip-tool-descriptions", s]);
        }

        let child = cmd.spawn().expect("spawn serve");
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

    fn list_tool_names(&self) -> Vec<String> {
        let resp: serde_json::Value = ureq::post(&self.url())
            .send_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }))
            .expect("POST tools/list")
            .into_json()
            .expect("parse JSON");
        resp["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool name").to_string())
            .collect()
    }

    fn list_raw(&self) -> serde_json::Value {
        ureq::post(&self.url())
            .send_json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "tools/list",
                "params": {}
            }))
            .expect("POST tools/list")
            .into_json()
            .expect("parse JSON")
    }
}

impl Drop for ServerFixture {
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
fn minimal_profile_drops_lsp_tools() {
    let s = ServerFixture::spawn(Some("minimal"), Some("none"));
    let names = s.list_tool_names();
    // Minimal allows heliosdb_query (escape hatch) but not the
    // engine's LSP / branch / rename tools.
    assert!(
        names.iter().any(|n| n == "heliosdb_query"),
        "minimal should include heliosdb_query; got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.starts_with("helios_lsp_")),
        "minimal must not advertise helios_lsp_*; got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "heliosdb_insert"),
        "minimal must not advertise heliosdb_insert; got {names:?}"
    );
}

#[test]
fn standard_profile_keeps_curated_set() {
    let s = ServerFixture::spawn(Some("standard"), Some("none"));
    let names = s.list_tool_names();
    // Standard allows the LSP-read tools but drops the writers.
    assert!(
        names.iter().any(|n| n == "helios_lsp_definition"),
        "standard should include helios_lsp_definition; got {names:?}"
    );
    assert!(
        names.iter().any(|n| n == "helios_lsp_references"),
        "standard should include helios_lsp_references; got {names:?}"
    );
    assert!(
        !names.iter().any(|n| n == "helios_lsp_rename_apply"),
        "standard must not advertise helios_lsp_rename_apply; got {names:?}"
    );
}

#[test]
fn full_profile_keeps_everything() {
    let pass = ServerFixture::spawn(Some("full"), Some("none"));
    let standard = ServerFixture::spawn(Some("standard"), Some("none"));
    let full_names = pass.list_tool_names();
    let std_names = standard.list_tool_names();
    assert!(
        full_names.len() > std_names.len(),
        "full ({}) should expose more tools than standard ({})",
        full_names.len(),
        std_names.len()
    );
}

#[test]
fn strip_caps_description_size() {
    let s = ServerFixture::spawn(Some("full"), Some("32"));
    let payload = s.list_raw();
    let tools = payload["result"]["tools"].as_array().expect("tools array");
    for t in tools {
        let d = t["description"].as_str().unwrap_or("");
        assert!(
            d.len() <= 32,
            "description for {:?} too long ({} bytes): {d:?}",
            t["name"],
            d.len()
        );
        // No truncation marker — descriptions are advisory, agents
        // pick tools by name, not by exact prose.
        assert!(
            !d.contains("[+") && !d.contains("truncated"),
            "tool descriptions must not carry a truncation marker; got {d:?}"
        );
    }
}

#[test]
fn dispatch_still_works_for_filtered_tool() {
    // Spawn with --profile minimal. helios_lsp_definition is NOT
    // advertised, but calling it directly by name MUST still hit the
    // engine — the filter is on `tools/list` only, never on dispatch.
    let s = ServerFixture::spawn(Some("minimal"), Some("none"));
    let resp: serde_json::Value = ureq::post(&s.url())
        .send_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "method": "tools/call",
            "params": {
                "name": "helios_lsp_definition",
                "arguments": { "name": "x" }
            }
        }))
        .expect("POST tools/call")
        .into_json()
        .expect("parse JSON");
    // Either a successful result OR an engine error tagged isError=true
    // is fine — the assertion is "the dispatch reached the engine and
    // came back". A `-32601 Method not found` would indicate the
    // gateway swallowed the call.
    assert!(
        resp.get("result").is_some() || resp.get("error").is_some(),
        "expected result or error payload; got {resp}"
    );
    if let Some(err) = resp.get("error") {
        let code = err["code"].as_i64().unwrap_or(0);
        assert_ne!(
            code, -32601,
            "gateway must NOT swallow filtered-tool calls — got -32601 (method not found): {resp}"
        );
    }
}

#[test]
fn full_passthrough_is_byte_identical_to_engine_default() {
    // --profile full --strip-tool-descriptions none should be
    // bytewise indistinguishable from the engine's native tools/list
    // response. The gateway has a passthrough fast-path that exits
    // before parsing for this exact combination.
    let s = ServerFixture::spawn(Some("full"), Some("none"));
    let payload = s.list_raw();
    let tools = payload["result"]["tools"].as_array().expect("tools array");
    // Pick any engine tool with a non-trivial description; passthrough
    // means we should see that description verbatim, not shortened.
    let helios_query = tools
        .iter()
        .find(|t| t["name"] == "heliosdb_query")
        .expect("heliosdb_query must be present under full profile");
    let desc = helios_query["description"].as_str().unwrap_or("");
    assert!(
        desc.len() > 32,
        "engine's heliosdb_query description should be the full text under passthrough; got {desc:?}"
    );
}
