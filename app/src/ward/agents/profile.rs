//! The machine profile: what the service hands this worker (`dnsProxy`, `mcp`,
//! `claudeSettings`, `skills`, `vault`) and how Ward puts it on the machine.
//!
//! The service keeps every part as it was written and only checks its shape; the parts are
//! parsed here. Applying is repeatable: Ward compares what is on the machine with what the
//! profile says and writes only the difference, so a version bumped by someone else's override
//! costs nothing.
//!
//! **Ward touches only its own entries.** The MCP servers it wrote, the settings keys it wrote,
//! the skills it wrote, the block between its markers in `CLAUDE.md` and in the `hosts` file are
//! remembered in [`Managed`] (`~/.claude/ward-agents-profile.json`). Everything else in those
//! files is left alone, and an entry that leaves the profile is taken out again. Being
//! disconnected from the service, the key removed or rejected, removes all of them
//! ([`remove_managed`]).
//!
//! The parts carry the secrets the service substituted for this machine, so no part content is
//! ever logged.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::contract::{PartReport, ProfileBody};
use super::machine::Machine;
use super::provider::Homes;

pub const PART_DNS_PROXY: &str = "dnsProxy";
pub const PART_MCP: &str = "mcp";
pub const PART_CLAUDE_SETTINGS: &str = "claudeSettings";
pub const PART_SKILLS: &str = "skills";
pub const PART_VAULT: &str = "vault";

/// Parts in the order Ward applies them: the proxy first (the vault may be cloned through it),
/// then the vault, then the agent's own configuration.
pub const PARTS: [&str; 5] = [
    PART_DNS_PROXY,
    PART_VAULT,
    PART_MCP,
    PART_CLAUDE_SETTINGS,
    PART_SKILLS,
];

/// `dnsProxy.mode` values this build applies. `hosts` writes the `entries` into the system
/// `hosts` file inside Ward's markers; `manual` only checks `checkUrl` and reports, for a proxy
/// installed outside Ward. Ward runs no program named by the profile: an unknown mode is
/// reported instead of executed.
pub const MODE_OFF: &str = "off";
pub const MODE_MANUAL: &str = "manual";
pub const MODE_HOSTS: &str = "hosts";

/// Ward's own record of what it wrote, next to the agent's configuration.
pub const STATE_FILE: &str = "ward-agents-profile.json";

const CLAUDE_MD_BEGIN: &str = "<!-- ward:agents:profile begin -->";
const CLAUDE_MD_END: &str = "<!-- ward:agents:profile end -->";
const HOSTS_BEGIN: &str = "# ward:agents:profile begin";
const HOSTS_END: &str = "# ward:agents:profile end";

const SETTINGS_FILE: &str = "settings.json";
const CLAUDE_MD_FILE: &str = "CLAUDE.md";
const SKILLS_DIR: &str = "skills";
const SKILL_FILE: &str = "SKILL.md";

/// The vault paths the orchestrator is told about when the profile does not say otherwise.
pub const DEFAULT_PLAYBOOK: &str = "_meta/playbooks/brain-manager.md";
pub const DEFAULT_PROMPT_TEMPLATE: &str = "_templates/task-prompt.md";
pub const DEFAULT_CONTEXT_PLAYBOOK: &str = "_meta/playbooks/task-context.md";
pub const DEFAULT_DOSSIER_DIR: &str = "11-Задачи/Активные";

/// `dnsProxy`: the DNS proxy client of this machine and how to check it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DnsProxy {
    pub enabled: bool,
    pub mode: String,
    pub check_url: String,
    /// `hosts`: host name to address.
    pub entries: BTreeMap<String, String>,
}

/// `mcp`: MCP servers for the user scope configuration of the agent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Mcp {
    pub servers: BTreeMap<String, Value>,
}

/// `claudeSettings`: keys of `~/.claude/settings.json` and the text of `~/.claude/CLAUDE.md`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ClaudeSettings {
    pub settings: Map<String, Value>,
    pub claude_md: String,
}

/// One shared skill: `~/.claude/skills/<name>/SKILL.md` plus whatever else it needs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Skill {
    pub name: String,
    /// The text of `SKILL.md`; `content` is taken as well.
    #[serde(alias = "content")]
    pub skill_md: String,
    /// More files of the skill by path inside its directory.
    pub files: BTreeMap<String, String>,
}

/// `skills`: the shared skills of the system.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Skills {
    pub skills: Vec<Skill>,
}

/// `vault`: where the vault is on this machine and how to refresh it. Without it the worker
/// takes no assignment that needs the preparation phase.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Vault {
    /// The service looks at this and at `path`: off, and the machine gets no assignment
    /// without a prompt.
    pub enabled: bool,
    /// Absolute path of the vault on this machine.
    pub path: String,
    /// Clone URL, used only when `path` is not there yet.
    pub repo: String,
    /// The branch the vault is kept on; empty: whatever is checked out.
    pub branch: String,
    /// `pull` (the default), `fetch` or `none`.
    pub update: String,
    /// Paths inside the vault for the orchestrator's prompt; empty means the default.
    pub playbook: String,
    pub prompt_template: String,
    pub context_playbook: String,
    pub dossier_dir: String,
    /// The agent of the orchestrator session; empty: the agent of the session itself.
    pub provider: String,
    /// The model of the orchestrator session; `None`: the agent's default.
    pub model: Option<String>,
}

impl Vault {
    pub fn dir(&self) -> PathBuf {
        PathBuf::from(self.path.trim())
    }

    /// The vault can be worked in: switched on in the profile, with a path that is there.
    pub fn is_ready(&self) -> bool {
        self.enabled && !self.path.trim().is_empty() && self.dir().is_dir()
    }

    pub fn playbook(&self) -> &str {
        non_empty(&self.playbook).unwrap_or(DEFAULT_PLAYBOOK)
    }

    pub fn prompt_template(&self) -> &str {
        non_empty(&self.prompt_template).unwrap_or(DEFAULT_PROMPT_TEMPLATE)
    }

    pub fn context_playbook(&self) -> &str {
        non_empty(&self.context_playbook).unwrap_or(DEFAULT_CONTEXT_PLAYBOOK)
    }

    pub fn dossier_dir(&self) -> &str {
        non_empty(&self.dossier_dir).unwrap_or(DEFAULT_DOSSIER_DIR)
    }

    /// The vault's path with forward slashes, as the prompt shows it.
    pub fn root(&self) -> String {
        self.path
            .trim()
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_owned()
    }

    /// A path inside the vault with forward slashes, as the prompt shows it.
    pub fn inside(&self, relative: &str) -> String {
        format!("{}/{}", self.root(), relative.trim_start_matches('/'))
    }
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

/// Ward's own entries in the configuration of this machine.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Managed {
    /// The profile version these entries come from.
    pub version: u32,
    pub mcp_servers: Vec<String>,
    pub settings_keys: Vec<String>,
    pub skills: Vec<String>,
    pub claude_md: bool,
    pub hosts: bool,
}

/// The files the profile is applied to; temp directories in tests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// `~/.claude`.
    pub claude_dir: PathBuf,
    /// `~/.claude.json`, the user scope MCP servers of Claude Code.
    pub claude_json: PathBuf,
    /// The system `hosts` file.
    pub hosts: PathBuf,
}

impl Target {
    pub fn of_machine(homes: &Homes) -> Self {
        Self {
            claude_dir: homes.claude_dir.clone(),
            claude_json: homes.claude_json.clone(),
            hosts: system_hosts(),
        }
    }

    pub fn state(&self) -> PathBuf {
        self.claude_dir.join(STATE_FILE)
    }

    fn settings(&self) -> PathBuf {
        self.claude_dir.join(SETTINGS_FILE)
    }

    fn claude_md(&self) -> PathBuf {
        self.claude_dir.join(CLAUDE_MD_FILE)
    }

    fn skills(&self) -> PathBuf {
        self.claude_dir.join(SKILLS_DIR)
    }
}

pub fn system_hosts() -> PathBuf {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        PathBuf::from(root)
            .join("System32")
            .join("drivers")
            .join("etc")
            .join("hosts")
    } else {
        PathBuf::from("/etc/hosts")
    }
}

/// What came out of applying a profile: the report for `POST /profile/applied`, the URL to
/// probe and the vault for the preparation phase.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outcome {
    pub version: u32,
    pub ok: bool,
    pub message: String,
    pub parts: Vec<PartReport>,
    /// `dnsProxy.checkUrl` while the proxy is wanted; `None`: nothing to check.
    pub check_url: Option<String>,
    /// The profile asks for a working proxy: without it the worker takes no assignment.
    pub proxy_required: bool,
    pub vault: Option<Vault>,
}

/// Applies `profile` to this machine. Blocking (files, git, possibly one elevation request):
/// run it off the main thread.
pub fn apply(profile: &ProfileBody, target: &Target, machine: &dyn Machine) -> Outcome {
    let previous = read_managed(target);
    let mut managed = Managed {
        version: profile.version,
        ..Default::default()
    };
    let mut outcome = Outcome {
        version: profile.version,
        ok: true,
        ..Default::default()
    };

    let dns = part::<DnsProxy>(profile, PART_DNS_PROXY);
    let dns_part = dns.as_ref().and_then(|part| part.as_ref().ok()).cloned();
    let result = match dns {
        None => None,
        Some(Err(err)) => Some(Err(err)),
        Some(Ok(part)) => Some(apply_dns_proxy(
            &part,
            &previous,
            target,
            machine,
            &mut managed,
        )),
    };
    report(&mut outcome, PART_DNS_PROXY, result);
    if let Some(dns) = &dns_part {
        outcome.proxy_required = dns.enabled && dns.mode.trim() != MODE_OFF;
        outcome.check_url = non_empty(&dns.check_url)
            .filter(|_| outcome.proxy_required)
            .map(str::to_owned);
    }

    let vault = part::<Vault>(profile, PART_VAULT);
    let vault_part = vault.as_ref().and_then(|part| part.as_ref().ok()).cloned();
    let result = match vault {
        None => None,
        Some(Err(err)) => Some(Err(err)),
        Some(Ok(part)) => Some(apply_vault(&part)),
    };
    report(&mut outcome, PART_VAULT, result);
    outcome.vault = vault_part;

    let mcp = part::<Mcp>(profile, PART_MCP);
    let present = mcp.is_some();
    let result = match mcp {
        Some(Err(err)) => Err(err),
        Some(Ok(part)) => apply_mcp(Some(&part), &previous, target, &mut managed),
        None => apply_mcp(None, &previous, target, &mut managed),
    };
    report(&mut outcome, PART_MCP, present.then_some(result));

    let settings = part::<ClaudeSettings>(profile, PART_CLAUDE_SETTINGS);
    let present = settings.is_some();
    let result = match settings {
        Some(Err(err)) => Err(err),
        Some(Ok(part)) => apply_claude_settings(Some(&part), &previous, target, &mut managed),
        None => apply_claude_settings(None, &previous, target, &mut managed),
    };
    report(
        &mut outcome,
        PART_CLAUDE_SETTINGS,
        present.then_some(result),
    );

    let skills = part::<Skills>(profile, PART_SKILLS);
    let present = skills.is_some();
    let result = match skills {
        Some(Err(err)) => Err(err),
        Some(Ok(part)) => apply_skills(Some(&part), &previous, target, &mut managed),
        None => apply_skills(None, &previous, target, &mut managed),
    };
    report(&mut outcome, PART_SKILLS, present.then_some(result));

    if let Err(err) = write_managed(target, &managed) {
        outcome.ok = false;
        outcome.message = push_message(&outcome.message, &format!("состояние профиля: {err}"));
    }
    if outcome.message.is_empty() {
        outcome.message = if outcome.parts.is_empty() {
            "в профиле нет частей".to_owned()
        } else {
            format!("профиль версии {} применён", profile.version)
        };
    }
    outcome
}

/// Takes Ward's own entries out of the machine's configuration and forgets them: the key is
/// gone or the service refused it.
pub fn remove_managed(target: &Target, machine: &dyn Machine) -> Outcome {
    let outcome = apply(&ProfileBody::default(), target, machine);
    let _ = std::fs::remove_file(target.state());
    outcome
}

/// Whether Ward has anything of its own on this machine.
pub fn has_managed(target: &Target) -> bool {
    target.state().is_file()
}

fn report(outcome: &mut Outcome, name: &str, result: Option<Result<String, String>>) {
    let Some(result) = result else {
        return;
    };
    let (ok, message) = match result {
        Ok(message) => (true, message),
        Err(message) => (false, message),
    };
    if !ok {
        outcome.ok = false;
        outcome.message = push_message(&outcome.message, &format!("{name}: {message}"));
    }
    outcome.parts.push(PartReport {
        name: name.to_owned(),
        ok,
        message,
    });
}

fn push_message(existing: &str, add: &str) -> String {
    if existing.is_empty() {
        add.to_owned()
    } else {
        format!("{existing}; {add}")
    }
}

/// The part under `name`: `None` when the profile does not have it, an error when its contents
/// are not what the contract says.
fn part<T: serde::de::DeserializeOwned>(
    profile: &ProfileBody,
    name: &str,
) -> Option<Result<T, String>> {
    let content = profile.parts.get(name)?;
    if content.is_null() {
        return None;
    }
    Some(
        serde_json::from_value(content.clone())
            .map_err(|err| format!("часть не по контракту: {err}")),
    )
}

fn apply_dns_proxy(
    part: &DnsProxy,
    previous: &Managed,
    target: &Target,
    machine: &dyn Machine,
    managed: &mut Managed,
) -> Result<String, String> {
    let mode = part.mode.trim();
    if !part.enabled || mode == MODE_OFF {
        if previous.hosts {
            write_hosts(target, machine, None)?;
        }
        return Ok("прокси выключен в профиле".to_owned());
    }
    match mode {
        "" | MODE_MANUAL => {
            if previous.hosts {
                write_hosts(target, machine, None)?;
            }
            Ok("прокси поставлен вне Ward, проверяется по checkUrl".to_owned())
        }
        MODE_HOSTS => {
            if part.entries.is_empty() {
                if previous.hosts {
                    write_hosts(target, machine, None)?;
                }
                return Ok("режим hosts без записей: нечего прописывать".to_owned());
            }
            let body = hosts_body(&part.entries);
            let changed = write_hosts(target, machine, Some(&body))?;
            managed.hosts = true;
            Ok(format!(
                "hosts: записей {}, {}",
                part.entries.len(),
                if changed {
                    "обновлено"
                } else {
                    "уже на месте"
                }
            ))
        }
        other => Err(format!(
            "режим {other} эта сборка Ward не применяет; поддержаны off, manual и hosts"
        )),
    }
}

/// The lines of the `hosts` block, address first, as the file wants them.
pub fn hosts_body(entries: &BTreeMap<String, String>) -> String {
    entries
        .iter()
        .filter(|(host, address)| !host.trim().is_empty() && !address.trim().is_empty())
        .map(|(host, address)| format!("{} {}", address.trim(), host.trim()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Writes Ward's block in the `hosts` file; `None` removes it. Returns whether the file
/// changed. The file belongs to the system, so a refused plain write is retried with one
/// elevation request.
fn write_hosts(target: &Target, machine: &dyn Machine, body: Option<&str>) -> Result<bool, String> {
    let existing = std::fs::read_to_string(&target.hosts).unwrap_or_default();
    let text = block_replaced(&existing, HOSTS_BEGIN, HOSTS_END, body);
    if text == existing {
        return Ok(false);
    }
    match std::fs::write(&target.hosts, &text) {
        Ok(()) => Ok(true),
        Err(err) => machine
            .write_elevated(&target.hosts, &text)
            .map(|()| true)
            .map_err(|elevated| {
                format!(
                    "{} нужны права администратора ({err}): {elevated}",
                    target.hosts.display()
                )
            }),
    }
}

/// `existing` with the text between the markers replaced by `body`; `None` takes the block out.
/// Everything outside the markers stays exactly as it was.
pub fn block_replaced(existing: &str, begin: &str, end: &str, body: Option<&str>) -> String {
    let without = match (existing.find(begin), existing.find(end)) {
        (Some(start), Some(stop)) if stop > start => {
            let after = stop + end.len();
            let tail = existing[after..]
                .strip_prefix('\n')
                .unwrap_or(&existing[after..]);
            format!("{}{}", &existing[..start], tail)
        }
        _ => existing.to_owned(),
    };
    let Some(body) = body else {
        // Removing the block leaves the text as it was, not a growing tail of blank lines.
        let trimmed = without.trim_end_matches(['\n', '\r']);
        return if trimmed.is_empty() {
            String::new()
        } else {
            format!("{trimmed}\n")
        };
    };
    let mut text = without;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() && !text.ends_with("\n\n") {
        text.push('\n');
    }
    text.push_str(begin);
    text.push('\n');
    text.push_str(body.trim_end_matches('\n'));
    text.push('\n');
    text.push_str(end);
    text.push('\n');
    text
}

fn apply_mcp(
    part: Option<&Mcp>,
    previous: &Managed,
    target: &Target,
    managed: &mut Managed,
) -> Result<String, String> {
    let desired: BTreeMap<String, Value> = part
        .map(|part| part.servers.clone())
        .unwrap_or_default()
        .into_iter()
        .filter(|(name, _)| !name.trim().is_empty())
        .collect();
    let gone: Vec<String> = previous
        .mcp_servers
        .iter()
        .filter(|name| !desired.contains_key(*name))
        .cloned()
        .collect();
    managed.mcp_servers = desired.keys().cloned().collect();
    if desired.is_empty() && gone.is_empty() {
        return Ok("серверов MCP в профиле нет".to_owned());
    }
    let (mut root, original) = read_json_object(&target.claude_json)?;
    {
        let object = root
            .as_object_mut()
            .ok_or_else(|| format!("{} не объект JSON", target.claude_json.display()))?;
        let servers = object
            .entry("mcpServers".to_owned())
            .or_insert_with(|| Value::Object(Map::new()));
        let servers = servers
            .as_object_mut()
            .ok_or_else(|| format!("mcpServers в {} не объект", target.claude_json.display()))?;
        for name in &gone {
            servers.remove(name.as_str());
        }
        for (name, value) in &desired {
            servers.insert(name.clone(), value.clone());
        }
    }
    let changed = write_json_if_changed(&target.claude_json, &root, &original)?;
    Ok(format!(
        "серверов MCP: {}{}, {}",
        desired.len(),
        if gone.is_empty() {
            String::new()
        } else {
            format!(", снято {}", gone.len())
        },
        if changed {
            "обновлено"
        } else {
            "уже на месте"
        }
    ))
}

fn apply_claude_settings(
    part: Option<&ClaudeSettings>,
    previous: &Managed,
    target: &Target,
    managed: &mut Managed,
) -> Result<String, String> {
    let settings = part.map(|part| part.settings.clone()).unwrap_or_default();
    let claude_md = part
        .map(|part| part.claude_md.trim().to_owned())
        .unwrap_or_default();
    let gone: Vec<String> = previous
        .settings_keys
        .iter()
        .filter(|key| !settings.contains_key(key.as_str()))
        .cloned()
        .collect();
    managed.settings_keys = settings.keys().cloned().collect();
    let mut changes = Vec::new();

    if !settings.is_empty() || !gone.is_empty() {
        let (mut root, original) = read_json_object(&target.settings())?;
        {
            let object = root
                .as_object_mut()
                .ok_or_else(|| format!("{} не объект JSON", target.settings().display()))?;
            for key in &gone {
                object.remove(key.as_str());
            }
            for (key, value) in &settings {
                object.insert(key.clone(), value.clone());
            }
        }
        if write_json_if_changed(&target.settings(), &root, &original)? {
            changes.push(format!("settings.json: ключей {}", settings.len()));
        }
    }

    if !claude_md.is_empty() || previous.claude_md {
        let path = target.claude_md();
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let body = (!claude_md.is_empty()).then_some(claude_md.as_str());
        let text = block_replaced(&existing, CLAUDE_MD_BEGIN, CLAUDE_MD_END, body);
        if text != existing {
            write_atomic(&path, &text)?;
            changes.push("CLAUDE.md: блок профиля".to_owned());
        }
        managed.claude_md = !claude_md.is_empty();
    }

    Ok(if changes.is_empty() {
        format!("ключей {} и CLAUDE.md уже на месте", settings.len())
    } else {
        changes.join(", ")
    })
}

fn apply_skills(
    part: Option<&Skills>,
    previous: &Managed,
    target: &Target,
    managed: &mut Managed,
) -> Result<String, String> {
    let skills = part.map(|part| part.skills.clone()).unwrap_or_default();
    let mut errors = Vec::new();
    let mut written = 0;
    let mut names = BTreeSet::new();
    let root = target.skills();
    for skill in &skills {
        let name = skill.name.trim();
        if !is_safe_name(name) {
            errors.push(format!("имя скилла {name} не годится для каталога"));
            continue;
        }
        match write_skill(&root.join(name), skill) {
            Ok(changed) => {
                names.insert(name.to_owned());
                if changed {
                    written += 1;
                }
            }
            Err(err) => errors.push(format!("скилл {name}: {err}")),
        }
    }
    let mut removed = 0;
    for name in &previous.skills {
        if names.contains(name) || !is_safe_name(name) {
            continue;
        }
        let dir = root.join(name);
        if dir.is_dir() {
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => removed += 1,
                Err(err) => errors.push(format!("скилл {name} не удалён: {err}")),
            }
        }
    }
    managed.skills = names.into_iter().collect();
    if !errors.is_empty() {
        return Err(errors.join("; "));
    }
    Ok(format!(
        "скиллов {}, обновлено {written}, снято {removed}",
        managed.skills.len()
    ))
}

/// A skill's files; returns whether anything changed.
fn write_skill(dir: &Path, skill: &Skill) -> Result<bool, String> {
    let mut changed = false;
    std::fs::create_dir_all(dir).map_err(|err| err.to_string())?;
    if !skill.skill_md.trim().is_empty() {
        changed |= write_text_if_changed(&dir.join(SKILL_FILE), &skill.skill_md)?;
    }
    for (relative, text) in &skill.files {
        let path = safe_join(dir, relative)?;
        changed |= write_text_if_changed(&path, text)?;
    }
    Ok(changed)
}

/// `dir` joined with a relative path from the profile: no absolute paths, no `..`.
fn safe_join(dir: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = relative.trim().replace('\\', "/");
    let mut path = dir.to_path_buf();
    for segment in relative.split('/').filter(|segment| !segment.is_empty()) {
        if !is_safe_name(segment) {
            return Err(format!("путь {relative} не годится"));
        }
        path.push(segment);
    }
    if path == dir {
        return Err("путь файла скилла пустой".to_owned());
    }
    Ok(path)
}

fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `vault`: the vault must be on the machine; `update` says how to refresh it.
fn apply_vault(part: &Vault) -> Result<String, String> {
    if !part.enabled {
        return Ok("волт выключен в профиле: задания без промпта эта машина не берёт".to_owned());
    }
    if part.path.trim().is_empty() {
        return Err("путь к волту не задан".to_owned());
    }
    super::git::prepare_vault(
        &part.dir(),
        part.repo.trim(),
        part.branch.trim(),
        &part.update,
    )
}

fn read_json_object(path: &Path) -> Result<(Value, Value), String> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let value = if text.trim().is_empty() {
        Value::Object(Map::new())
    } else {
        serde_json::from_str(&text)
            .map_err(|err| format!("{} не разбирается: {err}", path.display()))?
    };
    Ok((value.clone(), value))
}

/// Writes the object when it differs from what was read, comparing values and not text: the
/// formatting of a file Ward did not write stays its own.
fn write_json_if_changed(path: &Path, value: &Value, original: &Value) -> Result<bool, String> {
    if value == original {
        return Ok(false);
    }
    let mut text = serde_json::to_string_pretty(value).map_err(|err| err.to_string())?;
    text.push('\n');
    write_atomic(path, &text)?;
    Ok(true)
}

fn write_text_if_changed(path: &Path, text: &str) -> Result<bool, String> {
    if std::fs::read_to_string(path).is_ok_and(|existing| existing == text) {
        return Ok(false);
    }
    write_atomic(path, text)?;
    Ok(true)
}

/// Writes next to the file and renames: a half-written `~/.claude.json` would cost the owner
/// the configuration of their agent.
fn write_atomic(path: &Path, text: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let temp = path.with_extension("ward-tmp");
    std::fs::write(&temp, text).map_err(|err| format!("{}: {err}", temp.display()))?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(err) => {
            // Windows keeps a destination locked now and then: fall back to a plain write.
            let result =
                std::fs::write(path, text).map_err(|_| format!("{}: {err}", path.display()));
            let _ = std::fs::remove_file(&temp);
            result
        }
    }
}

pub fn read_managed(target: &Target) -> Managed {
    std::fs::read_to_string(target.state())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn write_managed(target: &Target, managed: &Managed) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(managed).map_err(|err| err.to_string())?;
    text.push('\n');
    write_atomic(&target.state(), &text)
}

#[cfg(test)]
#[path = "profile_tests.rs"]
mod tests;
