//! Exercise the shipped names/send commands with isolated home and fake transport.
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

const SESSION: &str = "c4217d94-c60f-4ad7-9b6b-1b7b52976ea4";
const OTHER: &str = "d4217d94-c60f-4ad7-9b6b-1b7b52976ea4";
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!(
            "prov-t3-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&home).unwrap();
        let f = Self(home);
        let state = f.0.join(".local/state/provenance");
        fs::create_dir_all(state.join("live")).unwrap();
        for id in [SESSION, OTHER] {
            fs::write(
                state.join(format!("live/{id}.json")),
                json!({
                    "harness":"claude-code", "session_id":id, "app":"", "cwd":"/same-folder",
                    "pid":4294967295_u32, "pid_started":"Fri Nov 13 00:00:00 2026",
                    "last_event":"Stop", "updated":"2026-11-13T00:00:00Z", "branches":[],
                    "transcript_path":null, "permission_mode":null
                })
                .to_string(),
            )
            .unwrap();
        }
        fs::write(
            state.join("names.json"),
            json!({"version":1,"names":{"target":{
                "display_name":"Target","harness":"claude-code","session_id":SESSION,
                "registered_at":"2026-11-13T00:00:00Z","cwd_hint":"/same-folder"
            }}})
            .to_string(),
        )
        .unwrap();
        fs::create_dir_all(f.0.join(".t3/bin")).unwrap();
        let bin = f.0.join(".t3/bin/t3");
        fs::write(
            &bin,
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$HOME/transport\"\n",
        )
        .unwrap();
        fs::set_permissions(bin, fs::Permissions::from_mode(0o700)).unwrap();
        f.sql("CREATE TABLE orchestration_v2_projection_threads (thread_id TEXT PRIMARY KEY, active_provider_thread_id TEXT, archived_at TEXT, deleted_at TEXT); CREATE TABLE orchestration_v2_projection_provider_threads (provider_thread_id TEXT PRIMARY KEY, payload_json TEXT);");
        f.add("thread", "provider", SESSION, "strong", true);
        f
    }
    fn sql(&self, sql: &str) {
        let dir = self.0.join(".t3/userdata");
        fs::create_dir_all(&dir).unwrap();
        assert!(
            Command::new("sqlite3")
                .arg(dir.join("statev2.sqlite"))
                .arg(sql)
                .status()
                .unwrap()
                .success()
        );
    }
    fn add(&self, thread: &str, provider: &str, session: &str, strength: &str, active: bool) {
        let payload = json!({"appThreadId":thread,"nativeThreadRef":{"driver":"claudeAgent","nativeId":session,"strength":strength}});
        self.sql(&format!("INSERT OR IGNORE INTO orchestration_v2_projection_threads VALUES ('{thread}', NULL, NULL, NULL); INSERT INTO orchestration_v2_projection_provider_threads VALUES ('{provider}', '{payload}'); UPDATE orchestration_v2_projection_threads SET active_provider_thread_id = '{provider}' WHERE thread_id = '{thread}' AND {active};"));
    }
    fn command(&self, args: &[&str]) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_provenance"))
            .env_clear()
            .env("HOME", &self.0)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("PROVENANCE_MACHINE", "fixture")
            .args(args)
            .output()
            .unwrap()
    }
    fn availability(&self) -> String {
        let out = self.command(&["names", "--json"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice::<Value>(&out.stdout).unwrap()[0]["availability"]
            .as_str()
            .unwrap()
            .into()
    }
    fn refused(&self) {
        assert!(self.availability().starts_with("unavailable:"));
        let out = self.command(&["send", "@target", "never deliver"]);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("isn't running"));
        assert!(!self.0.join(".local/state/provenance/mail").exists());
        assert!(!self.0.join("transport").exists());
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn names_and_send_deliver_dead_pid_exact_active_t3_session() {
    let f = Fixture::new();
    assert_eq!(f.availability(), "available");
    let out = f.command(&["send", "@target", "hello exact session"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("T3 Code"));
    let args = fs::read_to_string(f.0.join("transport")).unwrap();
    assert!(args.starts_with("thread\nsend\n--base-dir\n"));
    assert!(args.contains("\nthread\n"));
    assert!(args.contains("hello exact session"));
    assert_eq!(
        fs::read_dir(f.0.join(".local/state/provenance/mail"))
            .unwrap()
            .count(),
        1
    );
}
#[test]
fn names_and_send_refuse_historical_t3_mapping() {
    let f = Fixture::new();
    f.sql("UPDATE orchestration_v2_projection_threads SET active_provider_thread_id = NULL;");
    f.refused();
}
#[test]
fn names_and_send_refuse_weak_t3_mapping() {
    let f = Fixture::new();
    f.sql("UPDATE orchestration_v2_projection_provider_threads SET payload_json = json_set(payload_json, '$.nativeThreadRef.strength', 'weak');");
    f.refused();
}
#[test]
fn names_and_send_refuse_ambiguous_t3_mapping() {
    let f = Fixture::new();
    f.add("second", "second-provider", SESSION, "strong", true);
    f.refused();
}
#[test]
fn names_and_send_refuse_switched_t3_mapping() {
    let f = Fixture::new();
    f.add("thread", "new-provider", OTHER, "strong", true);
    f.refused();
}
#[test]
fn names_and_send_refuse_archived_t3_mapping() {
    let f = Fixture::new();
    f.sql("UPDATE orchestration_v2_projection_threads SET archived_at = 'now';");
    f.refused();
}
#[test]
fn names_and_send_refuse_deleted_t3_mapping() {
    let f = Fixture::new();
    f.sql("UPDATE orchestration_v2_projection_threads SET deleted_at = 'now';");
    f.refused();
}
#[test]
fn names_and_send_refuse_different_session_in_same_folder() {
    let f = Fixture::new();
    f.sql(&format!("UPDATE orchestration_v2_projection_provider_threads SET payload_json = json_set(payload_json, '$.nativeThreadRef.nativeId', '{OTHER}');"));
    f.refused();
}
