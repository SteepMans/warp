//! Whether this machine takes an assignment, checked again on the worker because the service
//! decided on a heartbeat that may be stale, and which agent runs it.

use std::collections::BTreeMap;

use super::contract::{AgentInfo, Limits, Resources, UsageReport};
use super::launch::{self, RepoId};
use super::provider::{self, Provider};

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The machine's side of the decision.
#[derive(Clone, Debug, PartialEq)]
pub struct MachineState<'a> {
    pub limits: &'a Limits,
    /// Paused on the Agents screen (heartbeat response) or in Ward's settings.
    pub paused: bool,
    /// Sessions of this machine that still hold a slot.
    pub active_sessions: usize,
    pub resources: &'a Resources,
    pub agents: &'a BTreeMap<String, AgentInfo>,
    pub usage: &'a [UsageReport],
}

/// Checks the machine limits for one more session in `repo`. The error is the `reason` of the
/// refusal.
pub fn check_slot(state: &MachineState<'_>, repo: &RepoId) -> Result<(), String> {
    let limits = state.limits;
    if state.paused || limits.paused {
        return Err("worker is paused".to_owned());
    }
    let max_sessions = limits.max_sessions as usize;
    if state.active_sessions >= max_sessions {
        return Err(format!(
            "no free slot ({}/{max_sessions})",
            state.active_sessions
        ));
    }
    let ram_reserve = limits.ram_reserve_gb.max(0.0) * GIB;
    if (state.resources.ram_free_bytes as f64) < ram_reserve {
        return Err(format!(
            "not enough free memory ({:.1} GB free, {} GB reserved)",
            state.resources.ram_free_bytes as f64 / GIB,
            limits.ram_reserve_gb
        ));
    }
    let disk_min = limits.disk_min_free_gb.max(0.0) * GIB;
    if (state.resources.disk_free_bytes as f64) < disk_min {
        return Err(format!(
            "not enough free disk ({:.1} GB free, {} GB required)",
            state.resources.disk_free_bytes as f64 / GIB,
            limits.disk_min_free_gb
        ));
    }
    if limits.cpu_percent > 0.0 && state.resources.cpu_percent > limits.cpu_percent {
        return Err(format!(
            "CPU is busy ({:.0}% > {}%)",
            state.resources.cpu_percent, limits.cpu_percent
        ));
    }
    if !launch::is_repo_allowed(repo, &limits.allowed_repos) {
        return Err(format!("repository {repo} is not allowed on this worker"));
    }
    Ok(())
}

/// Providers allowed by `limits.providers`, most preferred first; an empty list allows all in
/// the default order.
fn allowed_providers(limits: &Limits) -> Vec<&'static dyn Provider> {
    let listed: Vec<&'static dyn Provider> = limits
        .providers
        .split(',')
        .map(str::trim)
        .filter_map(provider::by_id)
        .collect();
    if limits.providers.trim().is_empty() {
        provider::all().to_vec()
    } else {
        listed
    }
}

/// The highest known `usedPercent` of the provider's windows, `None` when none is known.
fn max_usage(provider: &str, usage: &[UsageReport]) -> Option<f64> {
    usage
        .iter()
        .filter(|report| report.provider == provider)
        .filter_map(|report| report.used_percent)
        .reduce(f64::max)
}

fn check_provider(state: &MachineState<'_>, provider: &dyn Provider) -> Result<(), String> {
    let id = provider.id();
    let info = state.agents.get(id).cloned().unwrap_or_default();
    if !info.installed {
        return Err(format!("{id} is not installed"));
    }
    if !info.logged_in {
        return Err(format!("{id} is not logged in"));
    }
    if let Some(used) = max_usage(id, state.usage)
        && used >= state.limits.usage_max_percent
    {
        return Err(format!(
            "{id} subscription usage is at {used:.0}% (limit {}%)",
            state.limits.usage_max_percent
        ));
    }
    Ok(())
}

/// The agent for an assignment's `provider`: the named one if it passes, for `auto` the
/// allowed one with the lowest usage (unknown usage last, ties by preference), for an empty
/// value `claude`.
pub fn choose_provider(
    requested: &str,
    state: &MachineState<'_>,
) -> Result<&'static dyn Provider, String> {
    let requested = requested.trim();
    let allowed = allowed_providers(state.limits);
    if requested.is_empty() || !requested.eq_ignore_ascii_case("auto") {
        let id = if requested.is_empty() {
            provider::claude::ID
        } else {
            requested
        };
        let provider = provider::by_id(id).ok_or_else(|| format!("unknown provider {id}"))?;
        if !allowed.iter().any(|allowed| allowed.id() == provider.id()) {
            return Err(format!("provider {id} is not allowed on this worker"));
        }
        check_provider(state, provider)?;
        return Ok(provider);
    }
    let mut candidates: Vec<(usize, Option<f64>, &'static dyn Provider)> = allowed
        .into_iter()
        .enumerate()
        .filter(|(_, provider)| check_provider(state, *provider).is_ok())
        .map(|(order, provider)| (order, max_usage(provider.id(), state.usage), provider))
        .collect();
    candidates.sort_by(|a, b| {
        let usage = |value: Option<f64>| value.unwrap_or(f64::INFINITY);
        usage(a.1).total_cmp(&usage(b.1)).then(a.0.cmp(&b.0))
    });
    candidates
        .first()
        .map(|(_, _, provider)| *provider)
        .ok_or_else(|| "no installed and logged in agent under its usage limit".to_owned())
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
