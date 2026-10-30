//! `provenance install` / `uninstall`: a git hook that stamps agent commits,
//! and Claude Code and Codex hooks that keep live session records.
//!
//! The git hook is a config-defined hook (`hook.provenance.*` in the global
//! git config), which git runs alongside each repository's own hooks, so
//! nothing in `.git/hooks` or `core.hooksPath` is displaced.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

const SESSION_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "Stop",
    "SessionEnd",
];

pub struct Paths {
    pub claude_settings: PathBuf,
    pub codex_hooks: PathBuf,
}

impl Paths {
    pub fn new(home: &Path) -> Self {
        Paths {
            claude_settings: home.join(".claude/settings.json"),
            codex_hooks: home.join(".codex/hooks.json"),
        }
    }
}

pub fn install(home: &Path, bin: &Path, dry_run: bool) -> Result<Vec<String>> {
    let p = Paths::new(home);
    let mut done = Vec::new();

    if !config_hooks_supported() {
        bail!(
            "this git doesn't run hooks defined in config (hook.<name>.command); \
             provenance needs a newer git for commit stamps"
        );
    }
    if !dry_run {
        git_global(&[
            "--replace-all",
            "hook.provenance.event",
            "prepare-commit-msg",
        ])?;
        git_global(&["hook.provenance.command", &format!("{} stamp", quote(bin))])?;
    }
    done.push(
        "git: hook.provenance in global config (stamps agent commits; repositories' own hooks still run)"
            .into(),
    );

    for (file, harness, label) in [
        (&p.claude_settings, "claude-code", "Claude Code"),
        (&p.codex_hooks, "codex", "Codex"),
    ] {
        let mut doc = read_json(file)?;
        let added = add_session_hooks(&mut doc, &format!("{} hook {harness}", quote(bin)));
        if added > 0 && !dry_run {
            write_json_with_backup(file, &doc)?;
        }
        done.push(format!(
            "{label}: {added} hook events added to {}",
            file.display()
        ));
    }
    Ok(done)
}

pub fn uninstall(home: &Path, dry_run: bool) -> Result<Vec<String>> {
    let p = Paths::new(home);
    let mut done = Vec::new();
    if git_global_get("hook.provenance.command").is_some() {
        if !dry_run {
            git_global(&["--remove-section", "hook.provenance"])?;
        }
        done.push("git: removed hook.provenance from global config".into());
    }
    for (file, label, positional) in [
        (&p.claude_settings, "Claude Code", false),
        (&p.codex_hooks, "Codex", true),
    ] {
        if !file.exists() {
            continue;
        }
        let mut doc = read_json(file)?;
        let removed = remove_session_hooks(&mut doc, positional);
        if removed > 0 && !dry_run {
            write_json_with_backup(file, &doc)?;
        }
        done.push(format!(
            "{label}: {removed} hook entries removed from {}",
            file.display()
        ));
    }
    Ok(done)
}

/// Append one hook group per event, after any existing groups: Codex keys its
/// hook approvals by position, so existing entries must keep their indices.
pub fn add_session_hooks(doc: &mut Value, command: &str) -> usize {
    let hooks = doc
        .as_object_mut()
        .map(|o| o.entry("hooks").or_insert_with(|| json!({})))
        .and_then(Value::as_object_mut);
    let Some(hooks) = hooks else { return 0 };
    let mut added = 0;
    for event in SESSION_EVENTS {
        let groups = hooks.entry(*event).or_insert_with(|| json!([]));
        let Some(groups) = groups.as_array_mut() else {
            continue;
        };
        if groups.iter().any(group_is_ours) {
            continue;
        }
        let mut group = json!({
            "hooks": [{ "type": "command", "command": command, "timeout": 5 }]
        });
        if *event == "PostToolUse" {
            group["matcher"] = json!("*");
        }
        groups.push(group);
        added += 1;
    }
    added
}

/// Removes provenance's hook commands, leaving sibling commands alone. Where
/// position matters (Codex keys approvals by event, group and command index),
/// a removed command or group that has later ones after it becomes a no-op
/// `true` command, so the later ones keep their positions and approvals.
pub fn remove_session_hooks(doc: &mut Value, positional: bool) -> usize {
    let Some(hooks) = doc.get_mut("hooks").and_then(Value::as_object_mut) else {
        return 0;
    };
    let mut removed = 0;
    for groups in hooks.values_mut().filter_map(Value::as_array_mut) {
        let count = groups.len();
        let mut keep = vec![true; count];
        for (i, group) in groups.iter_mut().enumerate() {
            let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            let owned = handlers.iter().filter(|h| is_ours(h)).count();
            if owned == 0 {
                continue;
            }
            removed += owned;
            if positional {
                // Commands after a removed one keep their index: replace it
                // with a no-op, and drop only trailing ones.
                let last_kept = handlers.iter().rposition(|h| !is_ours(h));
                for (j, h) in handlers.iter_mut().enumerate() {
                    if is_ours(h) && last_kept.is_some_and(|k| j < k) {
                        *h = noop();
                    }
                }
            }
            handlers.retain(|h| !is_ours(h));
            if handlers.is_empty() {
                if positional && i + 1 < count {
                    handlers.push(noop());
                } else {
                    keep[i] = false;
                }
            }
        }
        let mut k = keep.into_iter();
        groups.retain(|_| k.next().unwrap_or(true));
    }
    hooks.retain(|_, v| v.as_array().is_none_or(|a| !a.is_empty()));
    removed
}

fn noop() -> Value {
    json!({ "type": "command", "command": "true" })
}

/// A hook command written by `install`: `'<path>/provenance' hook <harness>`.
fn is_ours(handler: &Value) -> bool {
    handler["command"].as_str().is_some_and(|c| {
        let Some(rest) = c.strip_prefix('\'') else {
            return false;
        };
        let Some((bin, tail)) = rest.rsplit_once('\'') else {
            return false;
        };
        (bin == "provenance" || bin.ends_with("/provenance"))
            && matches!(tail, " hook claude-code" | " hook codex")
    })
}

fn group_is_ours(group: &Value) -> bool {
    group["hooks"]
        .as_array()
        .is_some_and(|hs| hs.iter().any(is_ours))
}

fn read_json(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(text) if text.trim().is_empty() => Ok(json!({})),
        Ok(text) => {
            serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_json_with_backup(path: &Path, doc: &Value) -> Result<()> {
    if path.exists() {
        let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        let backup = path.with_extension(format!("json.provenance-backup-{stamp}"));
        std::fs::copy(path, &backup).with_context(|| format!("backing up {}", path.display()))?;
    } else if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Keep the original file's mode (settings can hold credentials); new
    // files are private.
    let mode = std::fs::metadata(path).map(|m| m.permissions()).ok();
    let tmp = path.with_extension("json.provenance-tmp");
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all((serde_json::to_string_pretty(doc)? + "\n").as_bytes())?;
    }
    if let Some(mode) = mode {
        std::fs::set_permissions(&tmp, mode)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn git_global(args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(["config", "--global"])
        .args(args)
        .status()
        .context("running git config")?;
    if !status.success() {
        bail!("git config --global {} failed", args.join(" "));
    }
    Ok(())
}

fn git_global_get(key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--global", "--get", key])
        .output()
        .ok()?;
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !v.is_empty()).then_some(v)
}

/// Runs a throwaway config-defined hook to see whether this git supports them.
fn config_hooks_supported() -> bool {
    let dir = std::env::temp_dir().join(format!("provenance-probe-{}", std::process::id()));
    let marker = dir.join("ran");
    let ok = std::fs::create_dir_all(&dir).is_ok()
        && Command::new("git")
            .args(["init", "-q"])
            .arg(&dir)
            .status()
            .is_ok_and(|s| s.success())
        && Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(["-c", "hook.probe.event=pre-auto-gc", "-c"])
            .arg(format!("hook.probe.command=touch {}", quote(&marker)))
            .args(["hook", "run", "pre-auto-gc"])
            .output()
            .is_ok()
        && marker.exists();
    let _ = std::fs::remove_dir_all(&dir);
    ok
}

/// Single-quote for sh.
fn quote(p: &Path) -> String {
    format!("'{}'", p.display().to_string().replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_after_existing_hooks_and_is_idempotent() {
        let mut doc = json!({
            "model": "x",
            "hooks": { "SessionStart": [ { "hooks": [ { "type": "command", "command": "other" } ] } ] }
        });
        assert_eq!(
            add_session_hooks(&mut doc, "'/bin/provenance' hook codex"),
            5
        );
        assert_eq!(
            add_session_hooks(&mut doc, "'/bin/provenance' hook codex"),
            0
        );
        let start = doc["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(
            start[0]["hooks"][0]["command"], "other",
            "existing entry keeps index 0"
        );
        assert_eq!(
            start[1]["hooks"][0]["command"],
            "'/bin/provenance' hook codex"
        );
        assert_eq!(doc["hooks"]["PostToolUse"][0]["matcher"], "*");
        assert_eq!(doc["model"], "x");

        assert_eq!(remove_session_hooks(&mut doc, true), 5);
        assert_eq!(doc["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        assert!(doc["hooks"].get("Stop").is_none());
    }

    #[test]
    fn uninstall_keeps_later_codex_positions_and_sibling_commands() {
        let ours = "'/bin/provenance' hook codex";
        let mut doc = json!({ "hooks": { "SessionStart": [
            { "hooks": [ { "type": "command", "command": "first" } ] }
        ] } });
        add_session_hooks(&mut doc, ours);
        // Another tool appends after provenance, and a user adds a sibling command.
        doc["hooks"]["SessionStart"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "hooks": [ { "type": "command", "command": "later" } ] }));
        doc["hooks"]["Stop"][0]["hooks"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "type": "command", "command": "sibling" }));

        let mut codex = doc.clone();
        assert_eq!(remove_session_hooks(&mut codex, true), 5);
        let start = codex["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 3);
        assert_eq!(start[1]["hooks"][0]["command"], "true", "slot kept");
        assert_eq!(start[2]["hooks"][0]["command"], "later", "keeps index 2");
        let stop = codex["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(stop[0]["command"], "true", "command slot kept");
        assert_eq!(stop[1]["command"], "sibling", "keeps command index 1");
        assert!(codex["hooks"].get("SessionEnd").is_none());

        let mut claude = doc;
        remove_session_hooks(&mut claude, false);
        let start = claude["hooks"]["SessionStart"].as_array().unwrap();
        assert_eq!(start.len(), 2, "position doesn't matter for Claude Code");
        assert_eq!(start[1]["hooks"][0]["command"], "later");
        let stop = claude["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["command"], "sibling");
    }

    #[test]
    fn ownership_needs_the_exact_installed_shape() {
        assert!(is_ours(&json!({ "command": "'/x/provenance' hook codex" })));
        assert!(!is_ours(
            &json!({ "command": "echo provenance hook codex" })
        ));
        assert!(!is_ours(
            &json!({ "command": "'/x/not-provenance' hook codex" })
        ));
        assert!(!is_ours(
            &json!({ "command": "'/x/provenance' hook codex; rm -rf /" })
        ));
    }

    #[test]
    fn rewrite_keeps_file_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("prov-mode-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("settings.json");
        std::fs::write(&f, "{}").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        write_json_with_backup(&f, &json!({ "a": 1 })).unwrap();
        let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let fresh = dir.join("new.json");
        write_json_with_backup(&fresh, &json!({})).unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn quotes_paths_for_sh() {
        assert_eq!(
            quote(Path::new("/Users/o'neil/bin/provenance")),
            r"'/Users/o'\''neil/bin/provenance'"
        );
    }

    #[test]
    fn trailing_owned_commands_are_dropped_not_padded() {
        let mut doc = json!({ "hooks": { "Stop": [ { "hooks": [
            { "type": "command", "command": "user" },
            { "type": "command", "command": "'/b/provenance' hook codex" }
        ] } ] } });
        assert_eq!(remove_session_hooks(&mut doc, true), 1);
        let stop = doc["hooks"]["Stop"][0]["hooks"].as_array().unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(stop[0]["command"], "user");
    }
}
