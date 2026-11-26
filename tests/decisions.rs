//! Exercise opt-in and persistence through the shipped CLI with isolated local state.
use serde_json::Value;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "prov-decisions-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_provenance"));
        command
            .args(["decisions"])
            .args(args)
            .env("HOME", &self.0)
            .env("PROVENANCE_STATE_DIR", self.0.join("state"))
            .env("PROVENANCE_NO_UPDATE_CHECK", "1");
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Output {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    fn snapshot(&self, pr: &str) -> Value {
        serde_json::from_slice(&self.ok(&["list", pr, "--json"]).stdout).unwrap()
    }
    fn record_args<'a>(&self, pr: &'a str) -> Vec<&'a str> {
        vec![
            "record",
            pr,
            "--actor",
            "codex/fixture",
            "--what",
            "Use a local journal",
            "--why",
            "Keep recording private",
            "--evidence",
            "src/decisions.rs",
            "--evidence",
            "cargo test --test decisions",
            "--source",
            "independent",
            "--timing",
            "contemporaneous",
        ]
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_repository_opt_in_covers_all_prs_and_retains_history_after_disabling() {
    let f = Fixture::new();
    let pr = "Owner/Repo#42";
    assert_eq!(f.snapshot(pr)["enabled"], false);
    assert!(
        !f.0.join("state").exists(),
        "reading default state must not create it"
    );
    let denied = f.run(&f.record_args(pr));
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("disabled"));
    assert!(!f.0.join("state/decisions/history.json").exists());
    f.ok(&["enable", "https://github.com/Owner/Repo.git/"]);
    let status: Value =
        serde_json::from_slice(&f.ok(&["status", "OWNER/REPO", "--json"]).stdout).unwrap();
    assert_eq!(status["repo"], "owner/repo");
    assert_eq!(status["enabled"], true);
    f.ok(&f.record_args(pr));
    f.ok(&f.record_args("owner/repo#43"));
    assert_eq!(
        f.snapshot("owner/repo#43")["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    // A different spelling of the same PR must resolve to the same opt-in and history.
    let first = f.snapshot("https://github.com/owner/repo/pull/42/files");
    assert_eq!(first["pr"], "owner/repo#42");
    assert_eq!(first["entries"][0]["source"], "independent");
    assert_eq!(first["entries"][0]["timing"], "contemporaneous");
    assert_eq!(first["entries"][0]["evidence"].as_array().unwrap().len(), 2);
    assert_eq!(first["entries"][0]["actor"], "codex/fixture");
    assert!(first["entries"][0]["recorded_at"].as_str().is_some());
    assert_eq!(f.snapshot("owner/other#42")["enabled"], false);
    assert!(!f.run(&f.record_args("owner/other#42")).status.success());
    f.ok(&["enable", "owner/other"]);
    f.ok(&f.record_args("owner/other#42"));
    f.ok(&["disable", "owner/repo"]);
    assert!(!f.run(&f.record_args(pr)).status.success());
    assert!(!f.run(&f.record_args("owner/repo#43")).status.success());
    assert!(!f.run(&f.record_args("owner/repo#44")).status.success());
    f.ok(&f.record_args("owner/other#42"));
    assert_eq!(f.snapshot("owner/other#42")["enabled"], true);
    assert_eq!(
        f.snapshot("owner/other#42")["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        f.snapshot("owner/repo#43")["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let disabled = f.snapshot(pr);
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["entries"], first["entries"]);
    f.ok(&["enable", "owner/repo"]);
    let mut args = f.record_args(pr);
    let source = args.iter().position(|arg| *arg == "independent").unwrap();
    args[source] = "user-directed";
    let timing = args
        .iter()
        .position(|arg| *arg == "contemporaneous")
        .unwrap();
    args[timing] = "retrospective";
    f.ok(&args);
    f.ok(&f.record_args("owner/repo#44"));
    assert_eq!(f.snapshot("owner/repo#44")["enabled"], true);
    let history = f.snapshot(pr);
    assert_eq!(history["entries"][1]["id"], 2);
    assert_eq!(history["entries"][1]["source"], "user-directed");
    assert_eq!(history["entries"][1]["timing"], "retrospective");
    let path = f.0.join("state/decisions/history.json");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(f.0.join("state/decisions"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
}

#[test]
fn cli_concurrent_records_are_not_lost() {
    let f = Fixture::new();
    let pr = "owner/repo#1";
    f.ok(&["enable", "owner/repo"]);
    let mut children = Vec::new();
    for _ in 0..8 {
        children.push(
            f.command(&f.record_args(pr))
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let history = f.snapshot(pr);
    let entries = history["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 8);
    for (index, entry) in entries.iter().enumerate() {
        assert_eq!(entry["id"], index + 1);
    }
}

#[test]
fn cli_rejects_incomplete_reports_and_preserves_corrupt_history() {
    let f = Fixture::new();
    let pr = "owner/repo#1";
    f.ok(&["enable", "owner/repo"]);
    let mut args = f.record_args(pr);
    let why = args
        .iter()
        .position(|arg| *arg == "Keep recording private")
        .unwrap();
    args[why] = "  ";
    assert!(!f.run(&args).status.success());
    assert!(f.snapshot(pr)["entries"].as_array().unwrap().is_empty());
    assert!(!f.run(&["record", pr]).status.success());
    assert!(!f.run(&["enable", "../../escape#1"]).status.success());
    let path = f.0.join("state/decisions/history.json");
    for text in ["broken", r#"{"version":99,"prs":{}}"#] {
        fs::write(&path, text).unwrap();
        assert!(!f.run(&["enable", "owner/repo"]).status.success());
        assert_eq!(fs::read_to_string(&path).unwrap(), text);
        assert!(!f.run(&["list", pr]).status.success());
    }
}

#[test]
fn cli_repository_and_pr_numbers_can_use_the_current_repository() {
    let f = Fixture::new();
    let bin = f.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    fs::write(&gh, "#!/bin/sh\nprintf 'Owner/Repo\\n'\n").unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    for args in [vec!["enable"], f.record_args("9"), vec!["disable"]] {
        let out = f
            .command(&args)
            .env("PATH", &path)
            .current_dir(&f.0)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let history = f.snapshot("owner/repo#9");
    assert_eq!(history["enabled"], false);
    assert_eq!(history["entries"].as_array().unwrap().len(), 1);
    let out = f
        .command(&["status", "--json"])
        .env("PATH", &path)
        .current_dir(&f.0)
        .output()
        .unwrap();
    assert!(out.status.success());
    let status: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(status["repo"], "owner/repo");
    assert_eq!(status["enabled"], false);
    for repo in [
        "owner/repo#9",
        "https://example.com/owner/repo",
        "https://github.com/owner/repo/pull/9",
    ] {
        assert!(!f.run(&["enable", repo]).status.success());
    }
}

#[test]
fn cli_legacy_pr_consent_never_enables_repository_recording() {
    let f = Fixture::new();
    let pr = "owner/repo#1";
    f.ok(&["enable", "owner/repo"]);
    f.ok(&f.record_args(pr));
    let path = f.0.join("state/decisions/history.json");
    let mut legacy: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    legacy["version"] = 1.into();
    legacy["prs"][pr]["enabled"] = true.into();
    // Even a repository field added to the old schema cannot broaden old consent.
    fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    let original = fs::read(&path).unwrap();
    assert_eq!(f.snapshot(pr)["enabled"], false);
    assert_eq!(f.snapshot(pr)["entries"].as_array().unwrap().len(), 1);
    assert!(!f.run(&f.record_args(pr)).status.success());
    assert!(!f.run(&f.record_args("owner/repo#2")).status.success());
    assert_eq!(fs::read(&path).unwrap(), original);
    f.ok(&["enable", "owner/other"]);
    let migrated: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(migrated["version"], 2);
    assert_eq!(migrated["repos"]["owner/other"], true);
    assert!(migrated["repos"].get("owner/repo").is_none());
    assert_eq!(f.snapshot(pr)["enabled"], false);
    assert_eq!(f.snapshot(pr)["entries"], legacy["prs"][pr]["entries"]);
    f.ok(&["enable", "owner/repo"]);
    f.ok(&f.record_args(pr));
    assert_eq!(f.snapshot(pr)["entries"][1]["id"], 2);
}
