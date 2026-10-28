//! Continuing a restored conversation's agent session in a new sandbox.
//!
//! The CLI behind an ACP adapter persists its session as the same transcript its terminal runner
//! resumes from, and the run uploads it (see [`super::cli_transcript`]). A run that continues a
//! restored conversation writes that transcript back where the CLI looks for it and asks the
//! adapter to `session/resume` it, so the agent keeps its memory of earlier turns the way
//! `claude --resume` and `codex resume` do on the terminal transport. `session/load` is not used:
//! it replays the whole history as `session/update`s, which would be applied to the restored
//! native conversation a second time.
//!
//! When the session cannot be resumed the run opens a fresh one, and its first prompt carries a
//! bounded replay of the restored conversation instead.
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context as _, Result};

use super::super::claude_transcript::{ClaudeTranscriptEnvelope, rehydrate_claude_transcript};
use super::super::codex_transcript::{CodexTranscriptEnvelope, rehydrate_codex_transcript};
use super::super::fetch_transcript_envelope;
use super::cli_transcript::CliTranscriptKind;
use super::connection::AcpConnection;
use super::protocol::{
    self, McpServer, NewSessionParams, NewSessionResponse, ResumeSessionParams,
    ResumeSessionResponse, SessionCapabilities,
};
use crate::ai::agent::api::ServerConversationToken;
use crate::ai::agent_sdk::driver::AgentDriverError;
use crate::ai::attachment_utils::DownloadedAttachment;
use crate::server::server_api::harness_support::HarnessSupportClient;

/// Upper bound on the restored history replayed into a fresh session's first prompt. The newest
/// exchanges are kept.
const HISTORY_REPLAY_MAX_BYTES: usize = 32 * 1024;

/// Where the run's ACP session came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum SessionOrigin {
    /// The run started a new conversation.
    New,
    /// The restored conversation's agent session was resumed.
    Resumed,
    /// The restored conversation's agent session could not be resumed, so a fresh one was opened.
    ResumeMissed(ResumeMiss),
}

/// Why a restored conversation's agent session was not resumed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ResumeMiss {
    /// This client cannot restore the harness's session transcript.
    Unsupported,
    /// The previous run never uploaded a session transcript.
    NoTranscript,
    /// The transcript could not be downloaded, read, or written back.
    Rehydration(String),
    /// The agent does not announce `session/resume`.
    AgentCannotResume,
    /// The agent refused `session/resume`.
    Rejected(String),
}

impl std::fmt::Display for ResumeMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unsupported => write!(f, "the harness's session transcript cannot be restored"),
            Self::NoTranscript => write!(f, "no session transcript was saved"),
            Self::Rehydration(error) => write!(f, "the session transcript was unusable: {error}"),
            Self::AgentCannotResume => write!(f, "the agent does not support session/resume"),
            Self::Rejected(error) => write!(f, "the agent refused session/resume: {error}"),
        }
    }
}

/// Downloads the restored conversation's CLI session transcript and writes it back where the CLI
/// reads it, returning the session id to resume.
pub(super) async fn rehydrate_cli_session(
    kind: Option<CliTranscriptKind>,
    conversation_id: &ServerConversationToken,
    client: Arc<dyn HarnessSupportClient>,
    cwd: &Path,
) -> Result<String, ResumeMiss> {
    let cwd = cwd.to_path_buf();
    let rehydrated = match kind {
        Some(CliTranscriptKind::Claude) => {
            let mut envelope: ClaudeTranscriptEnvelope =
                fetch_transcript_envelope("claude", conversation_id, client)
                    .await
                    .map_err(miss_from_fetch)?;
            tokio::task::spawn_blocking(move || {
                rehydrate_claude_transcript(&mut envelope, &cwd).map(|_| envelope.uuid)
            })
            .await
        }
        Some(CliTranscriptKind::Codex) => {
            let mut envelope: CodexTranscriptEnvelope =
                fetch_transcript_envelope("codex", conversation_id, client)
                    .await
                    .map_err(miss_from_fetch)?;
            tokio::task::spawn_blocking(move || {
                rehydrate_codex_transcript(&mut envelope, &cwd).map(|_| envelope.session_id)
            })
            .await
        }
        Some(CliTranscriptKind::Pi) | None => return Err(ResumeMiss::Unsupported),
    };
    match rehydrated {
        Ok(Ok(session_id)) => Ok(session_id.to_string()),
        Ok(Err(error)) => Err(ResumeMiss::Rehydration(format!("{error:#}"))),
        Err(error) => Err(ResumeMiss::Rehydration(format!(
            "transcript rehydration task failed: {error}"
        ))),
    }
}

fn miss_from_fetch(error: AgentDriverError) -> ResumeMiss {
    match error {
        AgentDriverError::ConversationResumeStateMissing { .. } => ResumeMiss::NoTranscript,
        error => ResumeMiss::Rehydration(error.to_string()),
    }
}

/// The session the handshake should resume, given the rehydrated CLI session (`None` when the run
/// does not continue a restored conversation) and what the agent announced.
pub(super) fn session_to_resume(
    rehydrated: Option<Result<String, ResumeMiss>>,
    capabilities: &SessionCapabilities,
) -> Option<Result<String, ResumeMiss>> {
    rehydrated.map(|rehydrated| {
        rehydrated.and_then(|session_id| {
            if capabilities.supports_resume() {
                Ok(session_id)
            } else {
                Err(ResumeMiss::AgentCannotResume)
            }
        })
    })
}

/// Opens the run's ACP session: resumes `to_resume` (from [`session_to_resume`]) when there is
/// one, and otherwise, or when the agent refuses it, starts a new session.
pub(super) async fn open_session(
    connection: &AcpConnection,
    to_resume: Option<Result<String, ResumeMiss>>,
    cwd: String,
    mcp_servers: Vec<McpServer>,
) -> Result<(NewSessionResponse, SessionOrigin)> {
    let origin = match to_resume {
        None => SessionOrigin::New,
        Some(Err(miss)) => SessionOrigin::ResumeMissed(miss),
        Some(Ok(session_id)) => {
            let resumed = connection
                .request::<_, Option<ResumeSessionResponse>>(
                    protocol::METHOD_SESSION_RESUME,
                    ResumeSessionParams {
                        session_id: session_id.clone(),
                        cwd: cwd.clone(),
                        mcp_servers: mcp_servers.clone(),
                    },
                )
                .await;
            match resumed {
                Ok(resumed) => {
                    let session = resumed.unwrap_or_default().into_session(session_id);
                    return Ok((session, SessionOrigin::Resumed));
                }
                Err(error) => {
                    SessionOrigin::ResumeMissed(ResumeMiss::Rejected(format!("{error:#}")))
                }
            }
        }
    };
    let session = connection
        .request(
            protocol::METHOD_SESSION_NEW,
            NewSessionParams { cwd, mcp_servers },
        )
        .await
        .context("session/new failed")?;
    Ok((session, origin))
}

/// The restored conversation, rendered for a fresh session's first prompt: user and agent text
/// only, as `transcript` (from [`AIConversation::export_to_markdown`] without an action model)
/// has it, trimmed to the newest exchanges that fit [`HISTORY_REPLAY_MAX_BYTES`].
///
/// [`AIConversation::export_to_markdown`]: crate::ai::agent::conversation::AIConversation::export_to_markdown
pub(super) fn history_replay(transcript: &str) -> Option<String> {
    let transcript = transcript.trim();
    if transcript.is_empty() {
        return None;
    }
    let (kept, truncated) = if transcript.len() <= HISTORY_REPLAY_MAX_BYTES {
        (transcript, false)
    } else {
        let mut start = transcript.len() - HISTORY_REPLAY_MAX_BYTES;
        while !transcript.is_char_boundary(start) {
            start += 1;
        }
        let tail = &transcript[start..];
        // Start at an exchange boundary when one fits, so the replay opens on a user turn.
        let tail = tail
            .find("\n\nUSER:\n")
            .map_or(tail, |boundary| &tail[boundary + 2..]);
        (tail, true)
    };
    let omitted = if truncated {
        " Older turns were left out."
    } else {
        ""
    };
    Some(format!(
        "<restored_conversation>\nThis continues an earlier conversation whose agent session could \
         not be restored. The conversation so far, without tool output, follows.{omitted}\n\n\
         {kept}\n</restored_conversation>"
    ))
}

/// The task's downloaded files to hand a resumed session: only those `prompt_file_ids` names,
/// since the session already holds the files earlier turns brought. Every file when the server
/// did not say which belong to the prompt.
pub(super) fn attachments_for_resumed_session(
    attachments: &[DownloadedAttachment],
    prompt_file_ids: Option<&[String]>,
) -> Vec<DownloadedAttachment> {
    match prompt_file_ids {
        Some(file_ids) => attachments
            .iter()
            .filter(|attachment| file_ids.contains(&attachment.file_id))
            .cloned()
            .collect(),
        None => attachments.to_vec(),
    }
}

#[cfg(test)]
#[path = "resume_tests.rs"]
mod tests;
