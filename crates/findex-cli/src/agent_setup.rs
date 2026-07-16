use anyhow::{anyhow, Context, Result};
use clap::ValueEnum;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const SKILL: &str = include_str!("../../../SKILL.md");
const TOOLS_REFERENCE: &str = include_str!("../../../references/tools.md");
const PLAYBOOK_REFERENCE: &str = include_str!("../../../references/agent-playbook.md");
const OPERATIONS_REFERENCE: &str = include_str!("../../../references/operations.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AgentTarget {
    All,
    Codex,
    Claude,
    Cursor,
    Antigravity,
}

#[derive(Debug, Serialize)]
pub struct SetupReport {
    pub agent: &'static str,
    pub mcp: String,
    pub skill: String,
    pub changed: bool,
}

pub fn setup(target: AgentTarget, force: bool, dry_run: bool) -> Result<Vec<SetupReport>> {
    let home = home_dir()?;
    let executable =
        std::env::current_exe().context("failed to locate the installed findex executable")?;
    let targets: &[AgentTarget] = match target {
        AgentTarget::All => &[
            AgentTarget::Codex,
            AgentTarget::Claude,
            AgentTarget::Cursor,
            AgentTarget::Antigravity,
        ],
        _ => std::slice::from_ref(&target),
    };
    targets
        .iter()
        .map(|target| setup_one(*target, &home, &executable, force, dry_run))
        .collect()
}

fn setup_one(
    target: AgentTarget,
    home: &Path,
    executable: &Path,
    force: bool,
    dry_run: bool,
) -> Result<SetupReport> {
    match target {
        AgentTarget::Codex => {
            let skill = home.join(".agents/skills/findex");
            let skill_changed = install_skill(&skill, force, dry_run)?;
            let args = mcp_cli_args(executable);
            let mcp =
                configure_command("codex", &["mcp", "remove", "findex"], &args, force, dry_run);
            Ok(report("codex", mcp, skill, skill_changed))
        }
        AgentTarget::Claude => {
            let skill = home.join(".claude/skills/findex");
            let skill_changed = install_skill(&skill, force, dry_run)?;
            let mut args = vec![
                OsString::from("mcp"),
                OsString::from("add"),
                OsString::from("findex"),
                OsString::from("--scope"),
                OsString::from("user"),
                OsString::from("--"),
            ];
            args.extend(mcp_server_args(executable));
            let mcp = configure_command(
                "claude",
                &["mcp", "remove", "findex", "--scope", "user"],
                &args,
                force,
                dry_run,
            );
            Ok(report("claude", mcp, skill, skill_changed))
        }
        AgentTarget::Cursor => {
            let skill = home.join(".agents/skills/findex");
            let skill_changed = install_skill(&skill, force, dry_run)?;
            let config = home.join(".cursor/mcp.json");
            let changed = merge_mcp_json(&config, executable, force, dry_run)?;
            Ok(SetupReport {
                agent: "cursor",
                changed: skill_changed || changed,
                mcp: config.display().to_string(),
                skill: skill.display().to_string(),
            })
        }
        AgentTarget::Antigravity => {
            let skill = home.join(".gemini/skills/findex");
            let skill_changed = install_skill(&skill, force, dry_run)?;
            let ide_config = home.join(".gemini/config/mcp_config.json");
            let cli_config = home.join(".gemini/antigravity-cli/mcp_config.json");
            let ide_changed = merge_mcp_json(&ide_config, executable, force, dry_run)?;
            let cli_changed = merge_mcp_json(&cli_config, executable, force, dry_run)?;
            Ok(SetupReport {
                agent: "antigravity",
                changed: skill_changed || ide_changed || cli_changed,
                mcp: format!("{}; {}", ide_config.display(), cli_config.display()),
                skill: skill.display().to_string(),
            })
        }
        AgentTarget::All => unreachable!("all targets are expanded before setup"),
    }
}

fn report(
    agent: &'static str,
    mcp: (bool, String),
    skill: PathBuf,
    skill_changed: bool,
) -> SetupReport {
    SetupReport {
        agent,
        changed: skill_changed || mcp.0,
        mcp: mcp.1,
        skill: skill.display().to_string(),
    }
}

fn home_dir() -> Result<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("could not determine the user home directory"))
}

fn mcp_server_args(executable: &Path) -> Vec<OsString> {
    vec![
        executable.as_os_str().to_owned(),
        OsString::from("--db-path"),
        OsString::from(".findex_db"),
        OsString::from("mcp"),
    ]
}

fn mcp_cli_args(executable: &Path) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("mcp"),
        OsString::from("add"),
        OsString::from("findex"),
        OsString::from("--"),
    ];
    args.extend(mcp_server_args(executable));
    args
}

fn configure_command(
    program: &str,
    remove_args: &[&str],
    add_args: &[OsString],
    force: bool,
    dry_run: bool,
) -> (bool, String) {
    let display = format_command(program, add_args);
    if dry_run {
        return (false, format!("dry-run: {display}"));
    }
    if force {
        let _ = Command::new(program).args(remove_args).output();
    }
    match Command::new(program).args(add_args).output() {
        Ok(output) if output.status.success() => (true, format!("configured with `{display}`")),
        Ok(output) => {
            let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if detail.is_empty() {
                format!("exited with {}", output.status)
            } else {
                detail
            };
            (
                false,
                format!("not changed ({detail}); run manually: {display}"),
            )
        }
        Err(error) => (
            false,
            format!("{program} CLI unavailable ({error}); run later: {display}"),
        ),
    }
}

fn format_command(program: &str, args: &[OsString]) -> String {
    let mut command = program.to_string();
    for arg in args {
        command.push(' ');
        let value = arg.to_string_lossy();
        if value.contains([' ', '\t']) {
            command.push('"');
            command.push_str(&value.replace('"', "\\\""));
            command.push('"');
        } else {
            command.push_str(&value);
        }
    }
    command
}

fn install_skill(directory: &Path, force: bool, dry_run: bool) -> Result<bool> {
    let files = [
        (Path::new("SKILL.md"), SKILL),
        (Path::new("references/tools.md"), TOOLS_REFERENCE),
        (
            Path::new("references/agent-playbook.md"),
            PLAYBOOK_REFERENCE,
        ),
        (Path::new("references/operations.md"), OPERATIONS_REFERENCE),
    ];
    let mut changed = false;
    for (relative, content) in files {
        let path = directory.join(relative);
        if path.exists() {
            let existing = fs::read_to_string(&path).with_context(|| {
                format!("failed to read existing skill file {}", path.display())
            })?;
            if existing == content {
                continue;
            }
            if !force {
                return Err(anyhow!(
                    "{} already exists with different content; inspect it or rerun with --force",
                    path.display()
                ));
            }
        }
        changed = true;
        if !dry_run {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&path, content)
                .with_context(|| format!("failed to write skill file {}", path.display()))?;
        }
    }
    Ok(changed && !dry_run)
}

fn merge_mcp_json(path: &Path, executable: &Path, force: bool, dry_run: bool) -> Result<bool> {
    let mut document = if path.exists() {
        serde_json::from_str::<Value>(&fs::read_to_string(path)?)
            .with_context(|| format!("{} is not valid JSON", path.display()))?
    } else {
        json!({})
    };
    let root = document
        .as_object_mut()
        .ok_or_else(|| anyhow!("{} must contain a JSON object", path.display()))?;
    let servers = root
        .entry("mcpServers")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| anyhow!("{}.mcpServers must be a JSON object", path.display()))?;
    let desired = json!({
        "command": executable,
        "args": ["--db-path", ".findex_db", "mcp"],
        "env": { "FINDEX_MODEL_POLICY": "auto" }
    });
    if let Some(existing) = servers.get("findex") {
        if existing == &desired {
            return Ok(false);
        }
        if !force {
            return Err(anyhow!(
                "{} already has a different findex server; inspect it or rerun with --force",
                path.display()
            ));
        }
    }
    servers.insert("findex".into(), desired);
    if dry_run {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        let backup = path.with_extension("json.findex-backup");
        fs::copy(path, &backup).with_context(|| {
            format!(
                "failed to back up {} to {}",
                path.display(),
                backup.display()
            )
        })?;
    }
    fs::write(path, serde_json::to_vec_pretty(&document)?)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_merge_preserves_other_servers_and_requires_force_for_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let config = temp.path().join("mcp.json");
        fs::write(&config, r#"{"mcpServers":{"other":{"command":"other"}}}"#).unwrap();
        let executable = std::env::current_exe().unwrap();
        assert!(merge_mcp_json(&config, &executable, false, false).unwrap());
        let value: Value = serde_json::from_slice(&fs::read(&config).unwrap()).unwrap();
        assert_eq!(value["mcpServers"]["other"]["command"], "other");
        assert_eq!(
            value["mcpServers"]["findex"]["args"],
            json!(["--db-path", ".findex_db", "mcp"])
        );
        fs::write(&config, r#"{"mcpServers":{"findex":{"command":"custom"}}}"#).unwrap();
        assert!(merge_mcp_json(&config, &executable, false, false).is_err());
        assert!(merge_mcp_json(&config, &executable, true, false).unwrap());
    }
}
