//! Ward, the fork's OSS build, turns the vertical tabs into a cockpit for CLI agents (Claude
//! Code, Codex): per-tab agent state, tabs grouped by project, an attention strip in the header
//! and jumps to agents waiting for the user. Everything here is a no-op outside the OSS build.

pub mod action;
pub mod agent_state;
pub mod attention;
pub mod changelog;
pub mod chimney;
pub mod claude_launch;
pub mod claude_usage;
pub mod labels;
pub mod metrics;
pub mod overview;
pub mod palette;
pub mod project_groups;
