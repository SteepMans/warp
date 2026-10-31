use std::collections::BTreeMap;

use super::*;

const GB: u64 = 1024 * 1024 * 1024;

fn resources() -> Resources {
    Resources {
        cpu_percent: 10.0,
        ram_free_bytes: 16 * GB,
        disk_free_bytes: 100 * GB,
    }
}

fn agents(entries: &[(&str, bool, bool)]) -> BTreeMap<String, AgentInfo> {
    entries
        .iter()
        .map(|(id, installed, logged_in)| {
            (
                (*id).to_owned(),
                AgentInfo {
                    installed: *installed,
                    version: None,
                    logged_in: *logged_in,
                    plan: None,
                },
            )
        })
        .collect()
}

fn usage(provider: &str, percent: Option<f64>) -> UsageReport {
    UsageReport {
        provider: provider.to_owned(),
        window: "w".to_owned(),
        window_minutes: None,
        used_percent: percent,
        resets_at: None,
        observed_at: None,
        source: "test".to_owned(),
    }
}

fn repo() -> RepoId {
    RepoId {
        owner: "SteepMans".to_owned(),
        name: "warp".to_owned(),
    }
}

#[test]
fn slot_checks() {
    let limits = Limits::default();
    let resources = resources();
    let agents = agents(&[]);
    let state = MachineState {
        limits: &limits,
        paused: false,
        active_sessions: 1,
        resources: &resources,
        agents: &agents,
        usage: &[],
    };
    assert_eq!(check_slot(&state, &repo()), Ok(()));

    let full = MachineState {
        active_sessions: 2,
        ..state.clone()
    };
    assert_eq!(
        check_slot(&full, &repo()),
        Err("no free slot (2/2)".to_owned())
    );

    let paused = MachineState {
        paused: true,
        ..state.clone()
    };
    assert_eq!(
        check_slot(&paused, &repo()),
        Err("worker is paused".to_owned())
    );
    let server_paused_limits = Limits {
        paused: true,
        ..Limits::default()
    };
    let server_paused = MachineState {
        limits: &server_paused_limits,
        ..state.clone()
    };
    assert!(check_slot(&server_paused, &repo()).is_err());

    let low = Resources {
        ram_free_bytes: 2 * GB,
        ..resources.clone()
    };
    let low_ram = MachineState {
        resources: &low,
        ..state.clone()
    };
    assert!(
        check_slot(&low_ram, &repo())
            .unwrap_err()
            .contains("memory")
    );

    let small = Resources {
        disk_free_bytes: 5 * GB,
        ..resources.clone()
    };
    let low_disk = MachineState {
        resources: &small,
        ..state.clone()
    };
    assert!(check_slot(&low_disk, &repo()).unwrap_err().contains("disk"));

    let busy = Resources {
        cpu_percent: 95.0,
        ..resources.clone()
    };
    let busy_cpu = MachineState {
        resources: &busy,
        ..state.clone()
    };
    assert!(check_slot(&busy_cpu, &repo()).unwrap_err().contains("CPU"));

    let only_other = Limits {
        allowed_repos: "SteepMans/chimney".to_owned(),
        ..Limits::default()
    };
    let not_allowed = MachineState {
        limits: &only_other,
        ..state
    };
    assert!(
        check_slot(&not_allowed, &repo())
            .unwrap_err()
            .contains("not allowed")
    );
}

#[test]
fn explicit_and_default_providers() {
    let limits = Limits::default();
    let resources = resources();
    let agents = agents(&[("claude", true, true), ("codex", true, false)]);
    let state = MachineState {
        limits: &limits,
        paused: false,
        active_sessions: 0,
        resources: &resources,
        agents: &agents,
        usage: &[],
    };
    assert_eq!(choose_provider("", &state).unwrap().id(), "claude");
    assert_eq!(choose_provider("claude", &state).unwrap().id(), "claude");
    assert_eq!(
        choose_provider("codex", &state).err(),
        Some("codex is not logged in".to_owned())
    );
    assert_eq!(
        choose_provider("gemini", &state).err(),
        Some("unknown provider gemini".to_owned())
    );

    let only_codex = Limits {
        providers: "codex".to_owned(),
        ..Limits::default()
    };
    let restricted = MachineState {
        limits: &only_codex,
        ..state.clone()
    };
    assert!(
        choose_provider("claude", &restricted)
            .err()
            .unwrap()
            .contains("not allowed")
    );

    let used = [usage("claude", Some(95.0))];
    let exhausted = MachineState {
        usage: &used,
        ..state
    };
    assert!(
        choose_provider("claude", &exhausted)
            .err()
            .unwrap()
            .contains("95%")
    );
}

#[test]
fn auto_picks_the_least_used_then_by_preference() {
    let limits = Limits::default();
    let resources = resources();
    let agents = agents(&[("claude", true, true), ("codex", true, true)]);
    let pick = |usage: &[UsageReport], limits: &Limits| {
        choose_provider(
            "auto",
            &MachineState {
                limits,
                paused: false,
                active_sessions: 0,
                resources: &resources,
                agents: &agents,
                usage,
            },
        )
        .map(|provider| provider.id())
    };
    assert_eq!(pick(&[], &limits), Ok("claude"));
    assert_eq!(
        pick(
            &[
                usage("claude", Some(60.0)),
                usage("codex", Some(20.0)),
                usage("codex", Some(30.0))
            ],
            &limits
        ),
        Ok("codex")
    );
    // Unknown usage goes after known values.
    assert_eq!(
        pick(
            &[usage("claude", None), usage("codex", Some(70.0))],
            &limits
        ),
        Ok("codex")
    );
    // Ties go by the order of `providers`.
    let codex_first = Limits {
        providers: "codex,claude".to_owned(),
        ..Limits::default()
    };
    assert_eq!(pick(&[], &codex_first), Ok("codex"));
    assert!(
        pick(
            &[usage("claude", Some(91.0)), usage("codex", Some(99.0))],
            &limits
        )
        .is_err()
    );
}
