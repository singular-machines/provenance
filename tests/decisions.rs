//! Exercise opt-in and persistence through the shipped CLI with isolated local state.
use serde_json::Value;
use std::{
    fs,
    io::Read,
    os::fd::FromRawFd,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Output, Stdio},
};
use unicode_width::UnicodeWidthStr;

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
            .current_dir(&self.0)
            .env("HOME", &self.0)
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
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
    fn snapshot(&self, target: &str) -> Value {
        serde_json::from_slice(&self.ok(&["list", target, "--json"]).stdout).unwrap()
    }
    fn text(&self, args: &[&str]) -> String {
        String::from_utf8(self.ok(args).stdout).unwrap()
    }
    fn record_args<'a>(&self, target: &'a str) -> Vec<&'a str> {
        vec![
            "record",
            target,
            "--actor",
            "codex/fixture",
            "--title",
            "Use a local journal",
            "--what",
            "Use a local journal",
            "--why",
            "Keep recording private",
            "--alternative",
            "Share reports externally",
            "This journal needs local privacy",
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

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn tty_output(f: &Fixture, args: &[&str], columns: u16, term: &str, no_color: bool) -> String {
    let mut master = -1;
    let mut slave = -1;
    let mut size = libc::winsize {
        ws_col: columns,
        ws_row: 32,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // openpty returns two owned descriptors on success.
    assert_eq!(
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &raw mut size,
            )
        },
        0
    );
    let mut input = unsafe { fs::File::from_raw_fd(master) };
    let terminal = unsafe { fs::File::from_raw_fd(slave) };
    let mut command = f.command(args);
    command
        .stdout(Stdio::from(terminal))
        .stderr(Stdio::piped())
        .env("TERM", term)
        .env_remove("NO_COLOR");
    if no_color {
        command.env("NO_COLOR", "1");
    }
    let child = command.spawn().unwrap();
    drop(command); // Close the parent's slave before reading for EOF.
    let mut output = Vec::new();
    if let Err(error) = input.read_to_end(&mut output) {
        // Linux reports EIO when the child closes the last slave descriptor.
        assert_eq!(error.raw_os_error(), Some(libc::EIO), "{error}");
    }
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(output).unwrap().replace("\r\n", "\n")
}

#[test]
fn cli_requires_concise_titles_and_keeps_full_decisions() {
    let f = Fixture::new();
    let repo = "owner/repo";
    let mut args = f.record_args(repo);
    let pos = args.iter().position(|arg| *arg == "--title").unwrap();
    let mut missing_title = args.clone();
    missing_title.drain(pos..pos + 2);
    assert!(!f.run(&missing_title).status.success());
    assert!(!f.0.join("state").exists());
    f.ok(&["enable", repo]);
    let path = f.0.join("state/decisions/history.json");
    let before = fs::read(&path).unwrap();
    let too_wide = "界".repeat(39);
    for title in [" ", "First\nSecond", "A\tB", "A\u{2028}B", &too_wide] {
        args[pos + 1] = title;
        let out = f.run(&args);
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr).contains("76 display columns"));
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    let widest = "界".repeat(38);
    args[pos + 1] = &widest;
    f.ok(&args);
    args[pos + 1] = "  Choose local storage  ";
    f.ok(&args);
    let snapshot = f.snapshot(repo);
    let entries = snapshot["entries"].as_array().unwrap();
    let entry = entries.iter().find(|entry| entry["id"] == 2).unwrap();
    assert_eq!(entry["title"], "Choose local storage");
    assert_eq!(entry["what"], "Use a local journal");
    assert_eq!(entry["why"], "Keep recording private");
    let timeline = f.text(&["list", repo]);
    assert!(timeline.lines().any(
        |line| line.contains("Choose local storage") && line.contains("Keep recording private")
    ));
    assert!(!timeline.contains("Use a local journal"));
    let show = f.text(&["show", repo, "2"]);
    assert!(show.starts_with(
        "Choose local storage\n  What: Use a local journal\n  Why: Keep recording private"
    ));
}

#[test]
fn cli_rejects_missing_incomplete_or_blank_alternative_pairs_before_writing() {
    let f = Fixture::new();
    let repo = "owner/repo";
    let args = f.record_args(repo);
    let start = args.iter().position(|arg| *arg == "--alternative").unwrap();
    let mut missing = args.clone();
    missing.drain(start..start + 3);
    let error = f.run(&missing);
    assert!(!error.status.success());
    assert!(String::from_utf8_lossy(&error.stderr).contains("--alternative"));
    assert!(!f.0.join("state").exists());

    f.ok(&["enable", repo]);
    let journal = f.0.join("state/decisions/history.json");
    let before = fs::read(&journal).unwrap();
    for (replacement, message) in [
        (&["--alternative", "Option only"][..], "--alternative"),
        (&["--alternative", " ", "Reason"][..], "must not be blank"),
        (
            &["--alternative", "Option", "\n\t"][..],
            "must not be blank",
        ),
        (
            &[
                "--alternative",
                "Option",
                "Reason",
                "--alternative",
                "Incomplete",
            ][..],
            "--alternative",
        ),
    ] {
        let mut invalid = args.clone();
        invalid.splice(start..start + 3, replacement.iter().copied());
        let out = f.run(&invalid);
        assert!(!out.status.success(), "{invalid:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(message));
        assert_eq!(fs::read(&journal).unwrap(), before);
    }
}

#[test]
fn cli_keeps_alternatives_separate_and_preserves_legacy_entries_when_linked() {
    let f = Fixture::new();
    let repo = "owner/repo";
    f.ok(&["enable", repo]);
    f.ok(&f.record_args(repo));
    let path = f.0.join("state/decisions/history.json");
    let mut journal: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    journal["repository_entries"][repo][0]
        .as_object_mut()
        .unwrap()
        .remove("alternatives");
    let legacy_bytes = serde_json::to_vec(&journal).unwrap();
    fs::write(&path, &legacy_bytes).unwrap();
    let mut legacy = f.snapshot(repo)["entries"][0].clone();
    let details = f.text(&["show", repo, "repo:1"]);
    assert!(!details.contains("Alternatives considered"));
    assert!(details.contains("Why: Keep recording private"));
    assert!(legacy.get("alternatives").is_none());
    assert_eq!(fs::read(&path).unwrap(), legacy_bytes);

    let mut args = f.record_args(repo);
    args.extend([
        "--alternative",
        "Defer to next release",
        "We need a private trail now",
    ]);
    f.ok(&args);
    let history = f.snapshot(repo);
    let recorded = history["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == 2)
        .unwrap();
    assert_eq!(
        recorded["alternatives"],
        serde_json::json!([
            { "option": "Share reports externally", "reason_not_selected": "This journal needs local privacy" },
            { "option": "Defer to next release", "reason_not_selected": "We need a private trail now" }
        ])
    );
    assert!(!f.text(&["list", repo]).contains("Defer to next release"));
    let details = f.text(&["show", repo, "repo:2"]);
    assert!(details.contains(
        "Alternatives considered:\n    - Share reports externally\n      Passed over: This journal needs local privacy\n    - Defer to next release\n      Passed over: We need a private trail now"
    ));

    f.ok(&["link", repo, "1", "--pr", "17"]);
    legacy["pr"] = "owner/repo#17".into();
    assert_eq!(f.snapshot("owner/repo#17")["entries"][0], legacy);
    assert!(
        !f.text(&["show", repo, "repo:1"])
            .contains("Alternatives considered")
    );
}

#[test]
fn cli_enable_installs_unrelated_content_safely_and_guidance_tracks_repository_scope() {
    let f = Fixture::new();
    let repo = "owner/repo";
    let codex = f.0.join(".codex");
    let claude = f.0.join(".claude/rules");
    fs::create_dir_all(&codex).unwrap();
    fs::create_dir_all(&claude).unwrap();
    fs::write(codex.join("AGENTS.md"), "Codex defaults\n").unwrap();
    let override_path = codex.join("AGENTS.override.md");
    fs::write(&override_path, "Keep my overriding instructions").unwrap();
    fs::set_permissions(&override_path, fs::Permissions::from_mode(0o640)).unwrap();
    fs::write(claude.join("my-rule.md"), "Claude preferences\n").unwrap();
    let claude_path = claude.join("provenance-decisions.md");
    let isolated = f.0.join("checkout");
    fs::create_dir(&isolated).unwrap();
    git(&isolated, &["init", "-q"]);
    git(
        &isolated,
        &["remote", "add", "origin", "git@github.com:Owner/Repo.git"],
    );
    let guide_at = |dir: &std::path::Path| {
        let result = f.command(&["guidance"]).current_dir(dir).output().unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        String::from_utf8(result.stdout).unwrap()
    };
    assert!(guide_at(&isolated).is_empty());
    assert!(!f.0.join("state").exists());
    f.ok(&["enable", repo]);
    let path = f.0.join("state/decisions/history.json");
    let initial_journal = fs::read(&path).unwrap();
    let codex_content = fs::read_to_string(&override_path).unwrap();
    let claude_content = fs::read_to_string(&claude_path).unwrap();
    for content in [&codex_content, &claude_content] {
        assert!(content.contains("provenance decisions guidance"));
        assert!(content.contains("Never enable recording yourself"));
        assert!(
            !content.contains(repo),
            "the user instructions are shared across opt-ins"
        );
    }
    assert!(codex_content.ends_with("Keep my overriding instructions"));
    assert_eq!(
        fs::metadata(&override_path).unwrap().permissions().mode() & 0o777,
        0o640
    );
    assert_eq!(
        fs::read_to_string(codex.join("AGENTS.md")).unwrap(),
        "Codex defaults\n"
    );
    assert_eq!(
        fs::read_to_string(claude.join("my-rule.md")).unwrap(),
        "Claude preferences\n"
    );
    f.ok(&["enable", repo]);
    assert_eq!(fs::read(&path).unwrap(), initial_journal);
    assert_eq!(fs::read_to_string(&override_path).unwrap(), codex_content);
    assert_eq!(fs::read_to_string(&claude_path).unwrap(), claude_content);
    let guidance = guide_at(&isolated);
    assert!(guidance.contains("recording is enabled for owner/repo"));
    assert!(guidance.contains("provenance decisions status owner/repo --json"));
    assert!(guidance.contains("--title <TITLE> --what <CHOICE> --why <REASON>"));
    assert!(guidance.contains("Record only material trade-offs"));
    assert!(guidance.contains("Skip routine implementation mechanics, status or verification recaps, and restatements of requirements"));
    assert!(guidance.contains("Use --what for the choice and affected scope"));
    assert!(guidance.contains("Flag material uncertainty"));
    assert!(guidance.contains("--evidence for verifiable references"));
    assert!(guidance.contains("--alternative <OPTION> <REASON_NOT_SELECTED>"));
    assert!(guidance.contains("Skip the report if no alternative was considered"));
    assert!(guidance.contains("leave out options merely suggested by later evidence"));
    let nested = isolated.join("repo");
    fs::create_dir(&nested).unwrap();
    git(&nested, &["init", "-q"]);
    git(
        &nested,
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/owner/repo-data",
        ],
    );
    assert!(guide_at(&nested).is_empty());
    git(
        &nested,
        &[
            "remote",
            "set-url",
            "origin",
            "https://github.com/owner/repo/elsewhere",
        ],
    );
    assert!(guide_at(&nested).is_empty());
    assert!(guide_at(&f.0).is_empty());
    assert_eq!(fs::read(&path).unwrap(), initial_journal);
    f.ok(&["disable", repo]);
    assert!(guide_at(&isolated).is_empty());
    assert_eq!(fs::read_to_string(&override_path).unwrap(), codex_content);
    assert_eq!(fs::read_to_string(&claude_path).unwrap(), claude_content);
}

#[test]
fn cli_enable_reports_conflicts_without_changing_opt_in_or_existing_guidance() {
    let f = Fixture::new();
    let codex = f.0.join("model-config");
    let claude = f.0.join("other-model-config");
    fs::create_dir_all(claude.join("rules")).unwrap();
    let foreign = claude.join("rules/provenance-decisions.md");
    fs::write(
        &foreign,
        "Unrelated instruction in a file with the same name.\n",
    )
    .unwrap();
    let enable = || {
        f.command(&["enable", "owner/repo"])
            .env("CODEX_HOME", &codex)
            .env("CLAUDE_CONFIG_DIR", &claude)
            .output()
            .unwrap()
    };
    let denied = enable();
    assert!(!denied.status.success());
    let error = String::from_utf8_lossy(&denied.stderr);
    assert!(error.contains("repository recording status is unchanged"));
    assert!(error.contains("provenance-decisions.md"));
    assert!(!codex.exists());
    assert!(!f.0.join("state/decisions/history.json").exists());
    assert_eq!(
        fs::read_to_string(&foreign).unwrap(),
        "Unrelated instruction in a file with the same name.\n"
    );
    fs::remove_file(&foreign).unwrap();
    assert!(enable().status.success());
    assert!(codex.join("AGENTS.md").is_file());
    assert!(foreign.is_file());
    assert!(!f.0.join(".codex").exists());
    assert!(!f.0.join(".claude").exists());
    assert_eq!(f.snapshot("owner/repo")["enabled"], true);
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
fn cli_lists_repository_and_pr_decisions_from_a_shipped_journal() {
    let f = Fixture::new();
    let repo = "owner/repo";
    let pr = "owner/repo#7";
    assert_eq!(f.snapshot(repo)["enabled"], false);
    assert!(!f.0.join("state").exists());
    assert!(!f.run(&f.record_args(repo)).status.success());

    let path = f.0.join("state/decisions/history.json");
    let legacy_entry = serde_json::json!({
        "id": 1,
        "recorded_at": "2025-01-01T00:00:00Z",
        "actor": "codex/old",
        "what": "Use a PR-backed report",
        "why": "The PR was already open",
        "evidence": ["one observation"],
        "source": "user-directed",
        "timing": "retrospective"
    });
    let shipped_journal = serde_json::json!({
        "version": 2,
        "repos": { "owner/repo": true, "owner/repo-2": true, "owner/disabled": false },
        "prs": { (pr): { "entries": [legacy_entry] } }
    });
    let original = serde_json::to_vec(&shipped_journal).unwrap();
    fs::write(&path, &original).unwrap();
    assert_eq!(f.snapshot(pr)["entries"][0], legacy_entry);
    let legacy_detail = f.text(&["show", pr, "1"]);
    assert!(legacy_detail.starts_with("Use a PR-backed report\n  Why: The PR was already open"));
    assert_eq!(f.snapshot(repo)["enabled"], true);
    assert_eq!(fs::read(&path).unwrap(), original);

    f.ok(&f.record_args("https://github.com/Owner/Repo.git/"));
    f.ok(&f.record_args("owner/repo#8"));
    f.ok(&f.record_args("owner/repo-2#7"));
    assert_eq!(f.snapshot(pr)["entries"][0], legacy_entry);
    assert_eq!(
        f.snapshot("owner/repo#8")["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let history = f.snapshot("OWNER/REPO");
    assert_eq!(history["repo"], repo);
    assert!(history.get("pr").is_none());
    let entries = history["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["target"], pr);
    assert_eq!(entries[0]["pr"], pr);
    assert!(
        entries
            .iter()
            .any(|entry| entry["target"] == "owner/repo#8")
    );
    let entry = entries
        .iter()
        .find(|entry| entry["target"] == repo)
        .unwrap();
    assert!(entry.get("pr").is_none());
    assert_eq!(entry["actor"], "codex/fixture");
    assert_eq!(entry["id"], 1);
    assert_eq!(entry["source"], "independent");
    assert_eq!(entry["timing"], "contemporaneous");
    assert_eq!(entry["evidence"].as_array().unwrap().len(), 2);
    let upgraded: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(upgraded["version"], 4);
    assert_eq!(upgraded["repos"], shipped_journal["repos"]);

    f.ok(&["disable", repo]);
    assert!(!f.run(&f.record_args(repo)).status.success());
    let disabled = f.snapshot(repo);
    assert_eq!(disabled["enabled"], false);
    assert_eq!(disabled["entries"], history["entries"]);
}

#[test]
fn cli_links_one_repository_decision_without_duplicating_or_rewriting_it() {
    let f = Fixture::new();
    let repo = "owner/repo";
    let pr = "owner/repo#7";
    let commit = "0123456789ABCDEF0123456789ABCDEF01234567";
    f.ok(&["enable", repo]);
    f.ok(&["enable", "owner/other"]);
    f.ok(&f.record_args(repo));
    f.ok(&f.record_args(pr));
    let original = f.snapshot(repo)["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["target"] == repo)
        .unwrap()
        .clone();
    assert_eq!(f.snapshot(pr)["entries"].as_array().unwrap().len(), 1);
    let path = f.0.join("state/decisions/history.json");
    let raw_before = fs::read(&path).unwrap();
    for args in [
        vec!["link", repo, "1"],
        vec!["link", repo, "2", "--pr", "7"],
        vec![
            "link",
            repo,
            "1",
            "--pr",
            "owner/other#7",
            "--commit",
            commit,
        ],
        vec!["link", repo, "1", "--pr", "7", "--commit", "invalid"],
    ] {
        assert!(!f.run(&args).status.success());
        assert_eq!(fs::read(&path).unwrap(), raw_before);
    }
    f.ok(&["link", repo, "1", "--commit", commit]);
    assert_eq!(f.snapshot(pr)["entries"].as_array().unwrap().len(), 1);
    f.ok(&["link", "Owner/Repo", "1", "--pr", "7"]);
    let raw_after = fs::read(&path).unwrap();
    f.ok(&[
        "link",
        repo,
        "1",
        "--pr",
        "https://github.com/owner/repo/pull/7",
        "--commit",
        commit,
    ]);
    assert_eq!(fs::read(&path).unwrap(), raw_after);
    assert!(!f.run(&["link", repo, "1", "--pr", "8"]).status.success());
    assert_eq!(fs::read(&path).unwrap(), raw_after);
    let history = f.snapshot(pr);
    let entries = history["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["id"], 1);
    assert!(entries[0].get("target").is_none());
    let mut linked = entries[1].clone();
    assert_eq!(linked["pr"], pr);
    assert_eq!(linked["commit"], commit.to_ascii_lowercase());
    linked.as_object_mut().unwrap().remove("pr");
    linked.as_object_mut().unwrap().remove("commit");
    assert_eq!(linked, original);
    assert_eq!(f.snapshot(repo)["entries"].as_array().unwrap().len(), 2);
    assert!(
        f.snapshot("owner/repo#8")["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    f.ok(&["disable", repo]);
    assert!(!f.run(&["link", repo, "1", "--pr", "7"]).status.success());
    assert_eq!(f.snapshot(pr)["entries"], history["entries"]);
}

#[test]
fn cli_human_lists_distinguish_empty_disabled_and_filtered_history() {
    let f = Fixture::new();
    let repo = "owner/repo";
    let output = f.text(&["list", repo]);
    assert!(output.contains("Recording disabled"));
    assert!(output.contains("No decisions recorded for this repository."));
    assert!(!f.0.join("state").exists());
    let implicit = f.run(&[]);
    assert!(!implicit.status.success());
    assert!(String::from_utf8_lossy(&implicit.stderr).contains("decisions list owner/repo"));
    f.ok(&["enable", repo]);
    let output = f.text(&["list", repo]);
    assert!(output.contains("Recording enabled"));
    assert!(output.contains("No decisions recorded for this repository."));
    assert!(
        f.text(&["list", "owner/repo#13"])
            .contains("No decisions match this PR.")
    );
    let mut report = f.record_args(repo);
    let index = report.iter().position(|arg| *arg == "independent").unwrap();
    report[index] = "user-directed";
    f.ok(&report);
    let output = f.text(&["list", repo, "--independent"]);
    assert!(output.contains("No independent decisions match this view."));
    assert!(!output.contains("Use a local journal"));
    f.ok(&["disable", repo]);
    let output = f.text(&["list", repo]);
    assert!(output.contains("Recording disabled"));
    assert!(output.contains("Use a local journal"));
}

#[test]
fn cli_recent_timeline_uses_local_git_and_lists_the_full_history_on_request() {
    let f = Fixture::new();
    for args in [
        &["init", "-q"][..],
        &["remote", "add", "origin", "git@github.com:Owner/Repo.git"],
        &["remote", "add", "upstream", "https://github.com/other/repo"],
    ] {
        let result = Command::new("git")
            .args(args)
            .current_dir(&f.0)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let bin = f.0.join("bin");
    fs::create_dir(&bin).unwrap();
    let gh = bin.join("gh");
    let called = f.0.join("gh-called");
    fs::write(&gh, "#!/bin/sh\ntouch \"$GH_CALLED\"\nexit 42\n").unwrap();
    fs::set_permissions(&gh, fs::Permissions::from_mode(0o700)).unwrap();
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let timeline = |args: &[&str]| {
        let result = f
            .command(args)
            .env("PATH", &path)
            .env("GH_CALLED", &called)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(!called.exists(), "implicit views must not invoke gh");
        String::from_utf8(result.stdout).unwrap()
    };
    let entries: Vec<_> = (1..=12)
        .map(|id| {
            serde_json::json!({
                "id": id,
                "recorded_at": format!("2026-01-{id:02}T13:14:15Z"),
                "actor": "codex/fixture",
                "what": format!("Choice {id:02}"),
                "why": "Human readable rationale",
                "evidence": ["details only"],
                "source": if id == 12 { "user-directed" } else { "independent" },
                "timing": if id == 11 { "retrospective" } else { "contemporaneous" },
            })
        })
        .collect();
    let journal = serde_json::json!({
        "version": 3,
        "repos": { "owner/repo": true },
        "prs": {},
        "repository_entries": { "owner/repo": entries },
    });
    let dir = f.0.join("state/decisions");
    fs::create_dir_all(&dir).unwrap();
    let bytes = serde_json::to_vec(&journal).unwrap();
    fs::write(dir.join("history.json"), &bytes).unwrap();

    let recent = timeline(&[]);
    assert!(recent.starts_with("owner/repo | Recording enabled"));
    assert!(recent.contains("Showing the newest 10 of 12 decisions."));
    assert!(recent.find("Choice 12").unwrap() < recent.find("Choice 11").unwrap());
    assert!(recent.contains("Time (UTC)") && recent.contains("Decision") && recent.contains("Why"));
    assert!(recent.contains("Human readable rationale"));
    assert!(recent.contains("2026-01-11"));
    assert!(recent.contains("13:14       repo:11"));
    assert!(recent.contains("Independent · Codex · No PR · Retrospective"));
    assert!(recent.contains("Details: provenance decisions show 'repo:12'"));
    assert_eq!(recent.matches("Details:").count(), 1);
    assert!(recent.contains("Full history: provenance decisions list owner/repo"));
    assert!(!recent.contains("Choice 01"));
    assert!(!recent.contains("details only"));
    assert!(!recent.contains("\u{1b}"));

    let filtered = timeline(&["--independent"]);
    assert!(filtered.contains("Choice 11"));
    assert!(filtered.contains("Choice 02"));
    assert!(filtered.contains("Full history: provenance decisions list owner/repo --independent"));
    assert!(!filtered.contains("Choice 12"));
    assert!(!filtered.contains("Choice 01"));
    let entire = timeline(&["list"]);
    assert!(entire.contains("Choice 01"));
    assert_eq!(entire, f.text(&["list", "owner/repo"]));
    let json = f.snapshot("owner/repo");
    assert_eq!(json["entries"][0]["what"], "Choice 01");
    assert_eq!(json["entries"][11]["what"], "Choice 12");
    let filtered_json: Value =
        serde_json::from_str(&f.text(&["list", "owner/repo", "--json", "--independent"])).unwrap();
    assert_eq!(filtered_json["entries"].as_array().unwrap().len(), 11);
    assert_eq!(filtered_json["entries"][10]["what"], "Choice 11");
    assert_eq!(fs::read(dir.join("history.json")).unwrap(), bytes);
}

#[test]
fn cli_detail_routes_preserve_distinct_targets_even_when_ids_collide() {
    let f = Fixture::new();
    let repo = "owner/repo";
    let pr = "owner/repo#7";
    let commit = "0123456789abcdef0123456789abcdef01234567";
    f.ok(&["enable", repo]);
    f.ok(&f.record_args(pr));
    for what in ["Linked repository choice", "Another repository choice"] {
        let mut report = f.record_args(repo);
        let title = report.iter().position(|arg| *arg == "--title").unwrap();
        report[title + 1] = what;
        let index = report.iter().position(|arg| *arg == "--what").unwrap();
        report[index + 1] = what;
        f.ok(&report);
    }
    f.ok(&["link", repo, "1", "--pr", "7", "--commit", commit]);
    f.ok(&["link", repo, "2", "--pr", "7"]);
    let path = f.0.join("state/decisions/history.json");
    let bytes = fs::read(&path).unwrap();
    let list = f.text(&["list", pr]);
    assert!(list.contains("#7:1"));
    assert!(list.contains("repo:1"));
    assert!(list.contains("repo:2"));
    assert!(list.contains("PR #7 · Commit 0123456789ab"));
    assert!(!list.contains(commit));
    let pr_record = f.text(&["show", pr, "1"]);
    assert!(pr_record.starts_with("Use a local journal\n  What:"));
    assert!(!pr_record.contains("Linked repository choice"));
    let repo_record = f.text(&["show", repo, "1"]);
    assert!(repo_record.starts_with("Linked repository choice\n  What:"));
    assert!(repo_record.contains(&format!("PR {pr} | Commit {commit} | Contemporaneous")));
    assert!(repo_record.contains("Actor: codex/fixture"));
    assert!(repo_record.contains("Recorded at:"));
    assert!(
        repo_record
            .contains("Evidence:\n    - src/decisions.rs\n    - cargo test --test decisions")
    );
    assert_eq!(f.text(&["show", repo, "repo:1"]), repo_record);
    assert_eq!(
        f.text(&["show", "https://github.com/owner/repo", "#7:1"]),
        pr_record
    );
    git(&f.0, &["init", "-q"]);
    git(
        &f.0,
        &["remote", "add", "origin", "git@github.com:owner/repo.git"],
    );
    assert_eq!(f.text(&["show", "repo:1"]), repo_record);
    assert_eq!(f.text(&["show", "#7:1"]), pr_record);
    for args in [
        vec!["show", pr, "repo:1"],
        vec!["show", repo, "#7:2"],
        vec!["show", repo, "#0:1"],
        vec!["show", "repo:bad"],
    ] {
        assert!(!f.run(&args).status.success());
    }
    let wrong_target = f.run(&["show", pr, "2"]);
    assert!(!wrong_target.status.success());
    assert!(String::from_utf8_lossy(&wrong_target.stderr).contains("use its original target"));
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn cli_decision_table_adapts_to_tty_width_and_honors_color_controls() {
    let f = Fixture::new();
    let repo = "owner/repo";
    f.ok(&["enable", repo]);
    let wide_word = "界".repeat(56);
    let why = format!(
        "Keep related words together so the rationale can be scanned in a narrow terminal.\nSecond physical line with 🙂.\n{wide_word}"
    );
    let mut report = f.record_args(repo);
    for (flag, value) in [
        (
            "--title",
            "Choose terminal widths that preserve both fields",
        ),
        ("--why", why.as_str()),
        ("--timing", "retrospective"),
    ] {
        let index = report.iter().position(|arg| *arg == flag).unwrap();
        report[index + 1] = value;
    }
    f.ok(&report);
    f.ok(&f.record_args("owner/repo#7"));
    let path = f.0.join("state/decisions/history.json");
    let mut fixture: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    fixture["repository_entries"][repo][0]["recorded_at"] = "2026-01-11T13:14:15Z".into();
    fixture["prs"]["owner/repo#7"]["entries"][0]["recorded_at"] = "2026-01-11T12:14:15Z".into();
    let bytes = serde_json::to_vec(&fixture).unwrap();
    fs::write(&path, &bytes).unwrap();

    let args = ["list", repo];
    let pipe = f.text(&args);
    assert!(!pipe.contains('\u{1b}'));
    assert!(
        pipe.lines()
            .any(|line| line.contains("Decision") && line.ends_with("Why"))
    );
    let color = tty_output(&f, &args, 110, "xterm-256color", false);
    assert!(color.contains("\x1b[2m13:14"));
    assert!(color.contains("\x1b[36mrepo:1"));
    assert!(color.contains("\x1b[1mChoose"));
    assert!(color.contains("\x1b[1;33mRetrospective"));

    let wide = tty_output(&f, &args, 250, "xterm-256color", true);
    assert!(!wide.contains('\u{1b}'));
    assert_eq!(wide.matches("\n2026-01-11\n").count(), 1);
    assert!(
        wide.lines()
            .any(|line| line.contains("Decision") && line.ends_with("Why"))
    );
    assert!(wide.lines().all(|line| line.width() <= 126));
    assert_eq!(wide.matches('界').count(), 56);
    let narrow = tty_output(&f, &args, 52, "dumb", false);
    assert!(!narrow.contains('\u{1b}'));
    assert!(narrow.contains("13:14  repo:1"));
    assert!(narrow.contains("  Why: Keep related words together"));
    assert!(narrow.contains("       Second physical line with 🙂."));
    assert!(narrow.lines().all(|line| line.width() <= 52));
    assert_eq!(narrow.matches('界').count(), 56);
    assert!(narrow.contains("Retrospective"));
    assert_eq!(fs::read(&path).unwrap(), bytes);
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
    assert_eq!(migrated["version"], 4);
    assert_eq!(migrated["repos"]["owner/other"], true);
    assert!(migrated["repos"].get("owner/repo").is_none());
    assert_eq!(f.snapshot(pr)["enabled"], false);
    assert_eq!(f.snapshot(pr)["entries"], legacy["prs"][pr]["entries"]);
    f.ok(&["enable", "owner/repo"]);
    f.ok(&f.record_args(pr));
    assert_eq!(f.snapshot(pr)["entries"][1]["id"], 2);
}
