//! Applying the profile against temp directories instead of the real home and `hosts` file.

use parking_lot::Mutex;
use serde_json::json;

use super::super::contract::Resources;
use super::super::launch::RunnerKind;
use super::super::machine::Hardware;
use super::*;

/// Records the elevated writes and refuses them when told to, the way a declined UAC request
/// looks from here.
struct FakeMachine {
    allow_elevated: bool,
    elevated: Mutex<Vec<(PathBuf, String)>>,
}

impl FakeMachine {
    fn new(allow_elevated: bool) -> Self {
        Self {
            allow_elevated,
            elevated: Mutex::new(Vec::new()),
        }
    }
}

impl Machine for FakeMachine {
    fn hardware(&self, _root: &Path) -> Hardware {
        Hardware::default()
    }

    fn resources(&self, _root: &Path) -> Resources {
        Resources::default()
    }

    fn has_tool(&self, _name: &str) -> bool {
        false
    }

    fn tool_version(&self, _name: &str) -> Option<String> {
        None
    }

    fn is_alive(&self, _pid: u32) -> bool {
        false
    }

    fn stop(&self, _pids: &[u32]) {}

    fn runner(&self) -> Option<(RunnerKind, Option<PathBuf>)> {
        Some((RunnerKind::Pwsh, None))
    }

    fn write_elevated(&self, path: &Path, contents: &str) -> Result<(), String> {
        self.elevated
            .lock()
            .push((path.to_path_buf(), contents.to_owned()));
        if self.allow_elevated {
            std::fs::write(path, contents).map_err(|err| err.to_string())
        } else {
            Err("в правах администратора отказано".to_owned())
        }
    }
}

struct Fixture {
    dir: tempfile::TempDir,
    target: Target,
    machine: FakeMachine,
}

fn fixture() -> Fixture {
    fixture_with_elevation(true)
}

fn fixture_with_elevation(allow: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let target = Target {
        claude_dir: home.join(".claude"),
        claude_json: home.join(".claude.json"),
        hosts: dir.path().join("system").join("hosts"),
    };
    std::fs::create_dir_all(target.hosts.parent().unwrap()).unwrap();
    Fixture {
        dir,
        target,
        machine: FakeMachine::new(allow),
    }
}

fn profile_body(parts: serde_json::Value, version: u32) -> ProfileBody {
    ProfileBody {
        version,
        parts: parts
            .as_object()
            .unwrap()
            .iter()
            .map(|(name, content)| (name.clone(), content.clone()))
            .collect(),
    }
}

fn part_report(outcome: &Outcome, name: &str) -> PartReport {
    outcome
        .parts
        .iter()
        .find(|part| part.name == name)
        .cloned()
        .unwrap_or_else(|| panic!("нет части {name} в {:?}", outcome.parts))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn mcp_servers_are_merged_and_foreign_ones_are_left_alone() {
    let fixture = fixture();
    std::fs::write(
        &fixture.target.claude_json,
        json!({
            "numStartups": 12,
            "mcpServers": {"grafana": {"url": "own"}}
        })
        .to_string(),
    )
    .unwrap();
    let profile = profile_body(
        json!({"mcp": {"servers": {"chimney": {"url": "https://chimney", "token": "t"}}}}),
        3,
    );
    let outcome = apply(&profile, &fixture.target, &fixture.machine);
    assert!(outcome.ok, "{outcome:?}");
    assert!(part_report(&outcome, PART_MCP).ok);

    let claude_json = read_json(&fixture.target.claude_json);
    // The owner's own keys and servers stay.
    assert_eq!(claude_json["numStartups"], 12);
    assert_eq!(claude_json["mcpServers"]["grafana"]["url"], "own");
    assert_eq!(claude_json["mcpServers"]["chimney"]["token"], "t");
    assert_eq!(read_managed(&fixture.target).mcp_servers, ["chimney"]);
    assert_eq!(read_managed(&fixture.target).version, 3);

    // The server leaves the profile: only Ward's own entry goes with it.
    let outcome = apply(
        &profile_body(json!({"mcp": {"servers": {}}}), 4),
        &fixture.target,
        &fixture.machine,
    );
    assert!(outcome.ok, "{outcome:?}");
    let claude_json = read_json(&fixture.target.claude_json);
    assert!(claude_json["mcpServers"].get("chimney").is_none());
    assert_eq!(claude_json["mcpServers"]["grafana"]["url"], "own");
    assert!(read_managed(&fixture.target).mcp_servers.is_empty());
}

#[test]
fn claude_settings_merge_keys_and_mark_their_block_in_claude_md() {
    let fixture = fixture();
    let settings = fixture.target.claude_dir.join("settings.json");
    let claude_md = fixture.target.claude_dir.join("CLAUDE.md");
    std::fs::write(&settings, json!({"theme": "dark"}).to_string()).unwrap();
    std::fs::write(&claude_md, "# Моё\n\nПравила владельца.\n").unwrap();

    let profile = profile_body(
        json!({"claudeSettings": {
            "settings": {"attribution": {"commit": "", "pr": ""}},
            "claudeMd": "## Общее\n\nБез эмодзи."
        }}),
        5,
    );
    let outcome = apply(&profile, &fixture.target, &fixture.machine);
    assert!(outcome.ok, "{outcome:?}");
    let written = read_json(&settings);
    assert_eq!(written["theme"], "dark");
    assert_eq!(written["attribution"]["commit"], "");
    let text = std::fs::read_to_string(&claude_md).unwrap();
    assert!(text.starts_with("# Моё\n\nПравила владельца.\n"), "{text}");
    assert!(text.contains("Без эмодзи."), "{text}");
    let managed = read_managed(&fixture.target);
    assert_eq!(managed.settings_keys, ["attribution"]);
    assert!(managed.claude_md);

    // The part leaves the profile: Ward's key and its block go, the owner's text stays.
    let outcome = apply(&ProfileBody::default(), &fixture.target, &fixture.machine);
    assert!(outcome.ok, "{outcome:?}");
    let written = read_json(&settings);
    assert_eq!(written["theme"], "dark");
    assert!(written.get("attribution").is_none());
    assert_eq!(
        std::fs::read_to_string(&claude_md).unwrap(),
        "# Моё\n\nПравила владельца.\n"
    );
    let managed = read_managed(&fixture.target);
    assert!(managed.settings_keys.is_empty());
    assert!(!managed.claude_md);
}

#[test]
fn skills_are_written_and_stale_ones_removed() {
    let fixture = fixture();
    let skills = fixture.target.claude_dir.join("skills");
    std::fs::create_dir_all(skills.join("own-skill")).unwrap();
    std::fs::write(skills.join("own-skill").join("SKILL.md"), "мой").unwrap();

    let profile = profile_body(
        json!({"skills": {"skills": [
            {"name": "bm-notify", "skillMd": "# bm-notify\n", "files": {"scripts/send.ps1": "ok"}},
            {"name": "vault-capture", "content": "# vault\n"}
        ]}}),
        6,
    );
    let outcome = apply(&profile, &fixture.target, &fixture.machine);
    assert!(outcome.ok, "{outcome:?}");
    assert_eq!(
        std::fs::read_to_string(skills.join("bm-notify").join("SKILL.md")).unwrap(),
        "# bm-notify\n"
    );
    assert_eq!(
        std::fs::read_to_string(skills.join("bm-notify").join("scripts").join("send.ps1")).unwrap(),
        "ok"
    );
    // `content` is taken as the text of SKILL.md as well.
    assert_eq!(
        std::fs::read_to_string(skills.join("vault-capture").join("SKILL.md")).unwrap(),
        "# vault\n"
    );
    assert_eq!(
        read_managed(&fixture.target).skills,
        ["bm-notify", "vault-capture"]
    );

    let outcome = apply(
        &profile_body(
            json!({"skills": {"skills": [{"name": "bm-notify", "skillMd": "# bm-notify\n"}]}}),
            7,
        ),
        &fixture.target,
        &fixture.machine,
    );
    assert!(outcome.ok, "{outcome:?}");
    assert!(!skills.join("vault-capture").exists());
    // The skill nobody told Ward about stays.
    assert!(skills.join("own-skill").join("SKILL.md").is_file());
}

#[test]
fn a_skill_with_an_unsafe_name_or_path_is_refused() {
    let fixture = fixture();
    let outcome = apply(
        &profile_body(
            json!({"skills": {"skills": [
                {"name": "../escape", "skillMd": "x"},
                {"name": "ok", "files": {"../../outside.txt": "x"}}
            ]}}),
            1,
        ),
        &fixture.target,
        &fixture.machine,
    );
    assert!(!outcome.ok);
    let report = part_report(&outcome, PART_SKILLS);
    assert!(!report.ok);
    assert!(report.message.contains("не годится"), "{report:?}");
    assert!(!fixture.target.claude_dir.join("escape").exists());
    assert!(!fixture.dir.path().join("outside.txt").exists());
}

#[test]
fn the_hosts_block_is_written_with_one_elevation_request_and_only_when_it_changes() {
    let fixture = fixture();
    std::fs::write(&fixture.target.hosts, "127.0.0.1 localhost\n").unwrap();
    let parts = json!({"dnsProxy": {
        "enabled": true,
        "mode": "hosts",
        "checkUrl": "https://api.anthropic.com/v1/models",
        "entries": {"api.anthropic.com": "10.0.0.2"}
    }});
    let outcome = apply(
        &profile_body(parts.clone(), 8),
        &fixture.target,
        &fixture.machine,
    );
    assert!(outcome.ok, "{outcome:?}");
    assert!(outcome.proxy_required);
    assert_eq!(
        outcome.check_url.as_deref(),
        Some("https://api.anthropic.com/v1/models")
    );
    let hosts = std::fs::read_to_string(&fixture.target.hosts).unwrap();
    assert!(hosts.starts_with("127.0.0.1 localhost\n"), "{hosts}");
    assert!(hosts.contains("10.0.0.2 api.anthropic.com"), "{hosts}");
    assert!(read_managed(&fixture.target).hosts);
    // The temp directory is writable, so no elevation was needed at all.
    assert!(fixture.machine.elevated.lock().is_empty());

    // Applying the same profile again changes nothing.
    let before = std::fs::read_to_string(&fixture.target.hosts).unwrap();
    let outcome = apply(&profile_body(parts, 8), &fixture.target, &fixture.machine);
    assert!(outcome.ok);
    assert!(
        part_report(&outcome, PART_DNS_PROXY)
            .message
            .contains("уже на месте")
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.target.hosts).unwrap(),
        before
    );

    // Switched off: the block goes, the owner's line stays.
    let outcome = apply(
        &profile_body(
            json!({"dnsProxy": {"enabled": false, "mode": "hosts", "checkUrl": "u"}}),
            9,
        ),
        &fixture.target,
        &fixture.machine,
    );
    assert!(outcome.ok, "{outcome:?}");
    assert!(!outcome.proxy_required);
    assert_eq!(outcome.check_url, None);
    assert_eq!(
        std::fs::read_to_string(&fixture.target.hosts).unwrap(),
        "127.0.0.1 localhost\n"
    );
    assert!(!read_managed(&fixture.target).hosts);
}

#[test]
fn a_refused_elevation_marks_the_machine_without_a_proxy() {
    let fixture = fixture_with_elevation(false);
    // A directory where the file cannot be created: the plain write fails like the system one.
    let hosts = fixture.target.hosts.join("inside").join("hosts");
    let target = Target {
        hosts: hosts.clone(),
        ..fixture.target.clone()
    };
    std::fs::write(&fixture.target.hosts, "127.0.0.1 localhost\n").unwrap();
    let outcome = apply(
        &profile_body(
            json!({"dnsProxy": {
                "enabled": true, "mode": "hosts", "checkUrl": "https://api.anthropic.com",
                "entries": {"api.anthropic.com": "10.0.0.2"}
            }}),
            10,
        ),
        &target,
        &fixture.machine,
    );
    assert!(!outcome.ok, "{outcome:?}");
    let report = part_report(&outcome, PART_DNS_PROXY);
    assert!(!report.ok);
    assert!(
        report.message.contains("права администратора"),
        "{report:?}"
    );
    assert_eq!(fixture.machine.elevated.lock().len(), 1);
    // The proxy is still wanted, so the worker knows it is without one.
    assert!(outcome.proxy_required);
    assert!(!read_managed(&target).hosts);
}

#[test]
fn the_manual_mode_only_asks_for_a_check() {
    let fixture = fixture();
    let outcome = apply(
        &profile_body(
            json!({"dnsProxy": {"enabled": true, "mode": "manual", "checkUrl": "https://u"}}),
            11,
        ),
        &fixture.target,
        &fixture.machine,
    );
    assert!(outcome.ok, "{outcome:?}");
    assert!(outcome.proxy_required);
    assert_eq!(outcome.check_url.as_deref(), Some("https://u"));
    assert!(!fixture.target.hosts.exists());
}

#[test]
fn an_unknown_proxy_mode_is_reported_not_run() {
    let fixture = fixture();
    let outcome = apply(
        &profile_body(
            json!({"dnsProxy": {"enabled": true, "mode": "install-resolver", "checkUrl": "u"}}),
            12,
        ),
        &fixture.target,
        &fixture.machine,
    );
    assert!(!outcome.ok);
    let report = part_report(&outcome, PART_DNS_PROXY);
    assert!(report.message.contains("install-resolver"), "{report:?}");
    assert!(fixture.machine.elevated.lock().is_empty());
}

#[test]
fn the_vault_part_keeps_its_paths_and_wants_a_directory() {
    let fixture = fixture();
    let vault_dir = fixture.dir.path().join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    let outcome = apply(
        &profile_body(
            json!({"vault": {
                "enabled": true,
                "path": vault_dir.to_string_lossy(),
                "update": "none",
                "provider": "claude",
                "model": "claude-opus-5-5"
            }}),
            13,
        ),
        &fixture.target,
        &fixture.machine,
    );
    let vault = outcome.vault.clone().unwrap();
    assert!(vault.is_ready());
    assert_eq!(vault.playbook(), DEFAULT_PLAYBOOK);
    assert_eq!(vault.prompt_template(), DEFAULT_PROMPT_TEMPLATE);
    assert_eq!(vault.dossier_dir(), DEFAULT_DOSSIER_DIR);
    assert_eq!(vault.model.as_deref(), Some("claude-opus-5-5"));
    // A directory of notes without git: the vault is usable, only updating is off.
    let report = part_report(&outcome, PART_VAULT);
    assert!(report.ok, "{report:?}");
    assert!(report.message.contains("не репозиторий git"), "{report:?}");

    // No path at all: the part is an error and the worker knows it has no vault.
    let outcome = apply(
        &profile_body(json!({"vault": {"enabled": true, "path": "  "}}), 14),
        &fixture.target,
        &fixture.machine,
    );
    assert!(!outcome.ok);
    assert!(!outcome.vault.unwrap().is_ready());
}

#[test]
fn a_part_that_is_not_what_the_contract_says_fails_alone() {
    let fixture = fixture();
    let outcome = apply(
        &profile_body(
            json!({
                "mcp": {"servers": "не объект"},
                "claudeSettings": {"settings": {"theme": "dark"}, "claudeMd": "текст"}
            }),
            15,
        ),
        &fixture.target,
        &fixture.machine,
    );
    assert!(!outcome.ok);
    assert!(!part_report(&outcome, PART_MCP).ok);
    // The other part was still applied.
    assert!(part_report(&outcome, PART_CLAUDE_SETTINGS).ok);
    assert_eq!(
        read_json(&fixture.target.claude_dir.join("settings.json"))["theme"],
        "dark"
    );
}

#[test]
fn applying_the_same_profile_twice_changes_nothing() {
    let fixture = fixture();
    let vault_dir = fixture.dir.path().join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    let parts = json!({
        "dnsProxy": {"enabled": true, "mode": "hosts", "checkUrl": "u",
                     "entries": {"api.anthropic.com": "10.0.0.2"}},
        "mcp": {"servers": {"chimney": {"url": "u"}}},
        "claudeSettings": {"settings": {"permissions": {"defaultMode": "auto"}},
                           "claudeMd": "## Общее\n"},
        "skills": {"skills": [{"name": "bm-notify", "skillMd": "# s\n"}]},
        "vault": {"enabled": true, "path": vault_dir.to_string_lossy(), "update": "none"}
    });
    let first = apply(
        &profile_body(parts.clone(), 20),
        &fixture.target,
        &fixture.machine,
    );
    let snapshot = |fixture: &Fixture| {
        let mut files: Vec<(String, String)> = Vec::new();
        for path in [
            fixture.target.claude_json.clone(),
            fixture.target.claude_dir.join("settings.json"),
            fixture.target.claude_dir.join("CLAUDE.md"),
            fixture
                .target
                .claude_dir
                .join("skills")
                .join("bm-notify")
                .join("SKILL.md"),
            fixture.target.hosts.clone(),
            fixture.target.state(),
        ] {
            if let Ok(text) = std::fs::read_to_string(&path) {
                files.push((path.to_string_lossy().into_owned(), text));
            }
        }
        files
    };
    let before = snapshot(&fixture);
    let second = apply(&profile_body(parts, 20), &fixture.target, &fixture.machine);
    assert_eq!(snapshot(&fixture), before);
    assert_eq!(first.ok, second.ok);
    assert_eq!(first.parts.len(), second.parts.len());
    assert!(fixture.machine.elevated.lock().is_empty());
}

#[test]
fn being_disconnected_removes_everything_of_ward() {
    let fixture = fixture();
    std::fs::write(
        &fixture.target.claude_json,
        json!({"mcpServers": {"grafana": {"url": "own"}}}).to_string(),
    )
    .unwrap();
    std::fs::write(fixture.target.claude_dir.join("CLAUDE.md"), "# Моё\n").unwrap();
    std::fs::write(&fixture.target.hosts, "127.0.0.1 localhost\n").unwrap();
    let outcome = apply(
        &profile_body(
            json!({
                "dnsProxy": {"enabled": true, "mode": "hosts", "checkUrl": "u",
                             "entries": {"api.anthropic.com": "10.0.0.2"}},
                "mcp": {"servers": {"chimney": {"url": "u"}}},
                "claudeSettings": {"settings": {"attribution": {"pr": ""}}, "claudeMd": "## Общее\n"},
                "skills": {"skills": [{"name": "bm-notify", "skillMd": "# s\n"}]}
            }),
            21,
        ),
        &fixture.target,
        &fixture.machine,
    );
    assert!(outcome.ok, "{outcome:?}");
    assert!(has_managed(&fixture.target));

    remove_managed(&fixture.target, &fixture.machine);
    assert!(!has_managed(&fixture.target));
    let claude_json = read_json(&fixture.target.claude_json);
    assert!(claude_json["mcpServers"].get("chimney").is_none());
    assert_eq!(claude_json["mcpServers"]["grafana"]["url"], "own");
    let settings = read_json(&fixture.target.claude_dir.join("settings.json"));
    assert!(settings.get("attribution").is_none());
    assert_eq!(
        std::fs::read_to_string(fixture.target.claude_dir.join("CLAUDE.md")).unwrap(),
        "# Моё\n"
    );
    assert_eq!(
        std::fs::read_to_string(&fixture.target.hosts).unwrap(),
        "127.0.0.1 localhost\n"
    );
    assert!(
        !fixture
            .target
            .claude_dir
            .join("skills")
            .join("bm-notify")
            .exists()
    );
}

#[test]
fn an_empty_profile_applies_cleanly() {
    let fixture = fixture();
    let outcome = apply(&ProfileBody::default(), &fixture.target, &fixture.machine);
    assert!(outcome.ok);
    assert!(outcome.parts.is_empty());
    assert!(!outcome.proxy_required);
    assert_eq!(outcome.message, "в профиле нет частей");
}

#[test]
fn the_block_helper_keeps_what_is_around_it() {
    let begin = "# begin";
    let end = "# end";
    let text = block_replaced("своё\n", begin, end, Some("наше"));
    assert_eq!(text, "своё\n\n# begin\nнаше\n# end\n");
    // Replacing keeps everything outside the markers.
    let text = block_replaced(&text, begin, end, Some("другое"));
    assert_eq!(text, "своё\n\n# begin\nдругое\n# end\n");
    assert_eq!(block_replaced(&text, begin, end, None), "своё\n");
    // Nothing to remove.
    assert_eq!(block_replaced("своё\n", begin, end, None), "своё\n");
    // An empty file gets the block without leading blank lines.
    assert_eq!(
        block_replaced("", begin, end, Some("наше")),
        "# begin\nнаше\n# end\n"
    );
}

#[test]
fn hosts_lines_are_address_first() {
    let entries = [
        ("api.anthropic.com".to_owned(), "10.0.0.2".to_owned()),
        ("  ".to_owned(), "10.0.0.3".to_owned()),
        ("empty.example".to_owned(), String::new()),
    ]
    .into_iter()
    .collect();
    assert_eq!(hosts_body(&entries), "10.0.0.2 api.anthropic.com");
}

#[test]
fn a_vault_switched_off_in_the_profile_is_not_used() {
    let fixture = fixture();
    let vault_dir = fixture.dir.path().join("vault");
    std::fs::create_dir_all(&vault_dir).unwrap();
    let outcome = apply(
        &profile_body(
            json!({"vault": {"enabled": false, "path": vault_dir.to_string_lossy()}}),
            16,
        ),
        &fixture.target,
        &fixture.machine,
    );
    assert!(outcome.ok, "{outcome:?}");
    assert!(
        part_report(&outcome, PART_VAULT)
            .message
            .contains("выключен")
    );
    assert!(!outcome.vault.unwrap().is_ready());
}
