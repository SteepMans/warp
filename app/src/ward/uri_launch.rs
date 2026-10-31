//! `warposs://action/new_tab?path=<dir>&command=<cmd>&title=<name>&shell=<name>&token=<t>&ack=<id>`:
//! opens a tab in `path`, names it `title` and runs `command` once the shell has bootstrapped.
//!
//! Any web page or link can trigger a custom URI scheme, so `command` runs only with
//! `token=<the contents of <state dir>/uri_token>` (`%LOCALAPPDATA%\warp\WarpOss\data\uri_token`
//! on Windows). Ward creates the file once, with 32 random bytes in hex, readable by the user
//! only. Without a valid token the tab still opens, without the command, and a warning is
//! logged. The token and the command text are never logged (the command length is).
//!
//! With `ack=<id>` (`[A-Za-z0-9_-]{1,64}`) Ward writes `<state dir>/uri_acks/<id>.json` once the
//! command was sent to the shell: `{"ok":true,"pane_uuid":"..","window_id":1,"ts":".."}`, or
//! `{"ok":false,"reason":"..",..}`. A script can poll for that file.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Serialize;
use url::Url;
use warpui::{AppContext, EntityId, SingletonEntity as _, WindowId};

use crate::pane_group::NewTerminalOptions;
use crate::terminal::available_shells::{AvailableShell, AvailableShells};
use crate::workspace::Workspace;

/// File name of the URI token in the state directory.
const TOKEN_FILE_NAME: &str = "uri_token";

/// Directory of the ack files in the state directory.
const ACKS_DIR_NAME: &str = "uri_acks";

/// Random bytes in a token; written as twice as many hex digits.
const TOKEN_BYTES: usize = 32;

const MAX_TITLE_CHARS: usize = 200;

/// A command not sent within this time (the shell never bootstrapped, the tab was closed) gets
/// an `ok:false` ack.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(120);

/// A valid `ack` id: `[A-Za-z0-9_-]{1,64}`, so it is safe as a file name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AckId(String);

impl AckId {
    pub fn parse(raw: &str) -> Option<Self> {
        let valid = (1..=64).contains(&raw.len())
            && raw
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-');
        valid.then(|| Self(raw.to_owned()))
    }
}

impl fmt::Display for AckId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The parameters of a Ward `new_tab` URI.
#[derive(Clone, PartialEq, Eq)]
pub struct NewTabRequest {
    pub path: PathBuf,
    pub command: Option<String>,
    pub title: Option<String>,
    pub shell: Option<String>,
    pub token: Option<String>,
    pub ack: Option<AckId>,
    /// `ack` was given but is not a valid id; no ack file is written.
    pub invalid_ack: bool,
}

/// Leaves out the token and the command text.
impl fmt::Debug for NewTabRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NewTabRequest")
            .field("path", &self.path)
            .field("command_len", &self.command.as_ref().map(String::len))
            .field("title", &self.title)
            .field("shell", &self.shell)
            .field("has_token", &self.token.is_some())
            .field("ack", &self.ack)
            .field("invalid_ack", &self.invalid_ack)
            .finish()
    }
}

/// Whether a `new_tab` URI uses any of the Ward parameters; plain `?path=` URIs keep the
/// upstream handling.
pub fn is_ward_new_tab(url: &Url) -> bool {
    url.query_pairs().any(|(key, _)| {
        matches!(
            key.as_ref(),
            "command" | "title" | "shell" | "token" | "ack"
        )
    })
}

fn query_value(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

pub fn parse_new_tab_request(url: &Url) -> Result<NewTabRequest, &'static str> {
    let path = query_value(url, "path").ok_or("missing path")?;
    if path.trim().is_empty() {
        return Err("missing path");
    }
    let path = PathBuf::from(shellexpand::tilde(&path).into_owned());
    let title = non_empty(query_value(url, "title"))
        .map(|title| title.trim().chars().take(MAX_TITLE_CHARS).collect());
    let raw_ack = query_value(url, "ack");
    let ack = raw_ack.as_deref().and_then(AckId::parse);
    Ok(NewTabRequest {
        path,
        command: non_empty(query_value(url, "command")),
        title,
        shell: non_empty(query_value(url, "shell")).map(|shell| shell.trim().to_owned()),
        token: non_empty(query_value(url, "token")).map(|token| token.trim().to_owned()),
        invalid_ack: raw_ack.is_some() && ack.is_none(),
        ack,
    })
}

/// Compares in time independent of where the inputs differ.
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenCheck {
    Valid,
    /// The URI has no `token`.
    Missing,
    /// The URI's token does not match.
    Mismatch,
    /// Ward has no token file to compare with.
    Unavailable,
}

impl TokenCheck {
    pub fn reason(self) -> &'static str {
        match self {
            Self::Valid => "ok",
            Self::Missing => "missing token",
            Self::Mismatch => "invalid token",
            Self::Unavailable => "no uri_token file",
        }
    }
}

pub fn check_token(provided: Option<&str>, expected: Option<&str>) -> TokenCheck {
    let Some(expected) = expected.filter(|expected| !expected.is_empty()) else {
        return TokenCheck::Unavailable;
    };
    let Some(provided) = provided else {
        return TokenCheck::Missing;
    };
    if constant_time_eq(provided.as_bytes(), expected.as_bytes()) {
        TokenCheck::Valid
    } else {
        TokenCheck::Mismatch
    }
}

fn is_valid_token(token: &str) -> bool {
    token.len() == TOKEN_BYTES * 2 && token.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `<state dir>/uri_token`.
pub fn token_path() -> PathBuf {
    warp_core::paths::state_dir().join(TOKEN_FILE_NAME)
}

/// `<state dir>/uri_acks`.
pub fn acks_dir() -> PathBuf {
    warp_core::paths::state_dir().join(ACKS_DIR_NAME)
}

/// The token in `path`, if the file holds one.
pub fn read_token(path: &Path) -> Option<String> {
    let token = std::fs::read_to_string(path).ok()?;
    let token = token.trim();
    is_valid_token(token).then(|| token.to_owned())
}

fn generate_token() -> String {
    use rand::RngCore as _;
    let mut bytes = [0u8; TOKEN_BYTES];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// Creates the token file unless it already holds a token, and returns the token. A file
/// without a valid token is replaced. On Unix the file is `0600`; on Windows it inherits the
/// ACL of the per-user `%LOCALAPPDATA%`, which only the user (and administrators) can read.
pub fn ensure_token(path: &Path) -> std::io::Result<String> {
    if let Some(token) = read_token(path) {
        return Ok(token);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let token = generate_token();
    let temp_path = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(&temp_path)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temp_path, path)?;
    Ok(token)
}

/// Creates the token file at startup, so scripts can read it before the first URI.
pub fn init() {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    if let Err(err) = ensure_token(&token_path()) {
        log::warn!("Ward: could not create the URI token file: {err}");
    }
}

/// The contents of an ack file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AckRecord {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_uuid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_id: Option<u64>,
    pub ts: String,
}

impl AckRecord {
    pub fn success(pane_uuid: Option<String>, window_id: Option<WindowId>) -> Self {
        Self {
            ok: true,
            reason: None,
            pane_uuid,
            window_id: window_id.and_then(window_number),
            ts: chrono::Utc::now().to_rfc3339(),
        }
    }

    pub fn failure(
        reason: impl Into<String>,
        pane_uuid: Option<String>,
        window_id: Option<WindowId>,
    ) -> Self {
        Self {
            ok: false,
            reason: Some(reason.into()),
            pane_uuid,
            window_id: window_id.and_then(window_number),
            ts: chrono::Utc::now().to_rfc3339(),
        }
    }
}

fn window_number(window_id: WindowId) -> Option<u64> {
    window_id.to_string().parse().ok()
}

/// Writes `<dir>/<id>.json` through a temporary file, so a polling script never reads half a
/// file.
pub fn write_ack(dir: &Path, id: &AckId, record: &AckRecord) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let json = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    let path = dir.join(format!("{id}.json"));
    let temp_path = dir.join(format!("{id}.json.tmp"));
    std::fs::write(&temp_path, json)?;
    std::fs::rename(&temp_path, &path)
}

fn write_ack_logged(id: &AckId, record: &AckRecord) {
    if let Err(err) = write_ack(&acks_dir(), id, record) {
        log::warn!("Ward: could not write URI ack {id}: {err}");
    }
}

/// An ack to write once the terminal view sends its pending command.
struct PendingAck {
    id: AckId,
    pane_uuid: Option<String>,
    window_id: WindowId,
}

thread_local! {
    static PENDING_ACKS: RefCell<HashMap<EntityId, PendingAck>> = RefCell::new(HashMap::new());
}

/// Writes the ack for `id` once `terminal_view_id` sends its pending command. Registered before
/// the command is queued, so a command sent right away is still confirmed.
pub fn expect_command_sent(
    terminal_view_id: EntityId,
    id: AckId,
    pane_uuid: Option<String>,
    window_id: WindowId,
) {
    PENDING_ACKS.with(|acks| {
        acks.borrow_mut().insert(
            terminal_view_id,
            PendingAck {
                id,
                pane_uuid,
                window_id,
            },
        );
    });
}

/// Called by the terminal view when it sent its pending command to the shell.
pub fn pending_command_sent(terminal_view_id: EntityId) {
    let pending = PENDING_ACKS.with(|acks| acks.borrow_mut().remove(&terminal_view_id));
    if let Some(pending) = pending {
        write_ack_logged(
            &pending.id,
            &AckRecord::success(pending.pane_uuid, Some(pending.window_id)),
        );
    }
}

/// Called [`ACK_TIMEOUT`] after the tab opened: an ack still pending fails.
pub fn expire_pending_ack(terminal_view_id: EntityId) {
    let pending = PENDING_ACKS.with(|acks| acks.borrow_mut().remove(&terminal_view_id));
    if let Some(pending) = pending {
        log::warn!(
            "Ward: the new_tab URI command was not sent within {} s",
            ACK_TIMEOUT.as_secs()
        );
        write_ack_logged(
            &pending.id,
            &AckRecord::failure(
                "command was not sent to the shell in time",
                pending.pane_uuid,
                Some(pending.window_id),
            ),
        );
    }
}

/// What the workspace should open, after the token check.
pub struct UriTab {
    pub options: NewTerminalOptions,
    pub title: Option<String>,
    /// Only when the token was valid.
    pub command: Option<String>,
    /// Confirmed once `command` was sent; only together with `command`.
    pub ack: Option<AckId>,
}

/// The new tab's persistent pane UUID (hex).
pub struct OpenedUriTab {
    pub pane_uuid: Option<String>,
}

/// Finds a shell by name: the short name (`PowerShell`, `Bash`), the command (`pwsh`,
/// `bash.exe`) or a WSL distribution (`Ubuntu`, `wsl:Ubuntu`), ignoring case.
pub(crate) fn find_shell(name: &str, app: &AppContext) -> Option<AvailableShell> {
    let wanted = name.trim().to_lowercase();
    let wanted_distro = wanted.strip_prefix("wsl:").unwrap_or(&wanted).to_owned();
    let strip_exe = |value: &str| value.trim_end_matches(".exe").to_owned();
    AvailableShells::as_ref(app)
        .get_available_shells()
        .find(|shell| {
            shell.short_name().to_lowercase() == wanted
                || strip_exe(&shell.telemetry_value().to_lowercase()) == strip_exe(&wanted)
                || shell
                    .wsl_distro()
                    .is_some_and(|distro| distro.to_lowercase() == wanted_distro)
        })
        .cloned()
}

/// Handles a Ward `new_tab` URI (see the module docs).
pub fn handle_new_tab(primary_window_id: Option<WindowId>, url: &Url, ctx: &mut AppContext) {
    let request = match parse_new_tab_request(url) {
        Ok(request) => request,
        Err(reason) => {
            log::warn!("Ward: ignoring a new_tab URI: {reason}");
            if let Some(ack) = query_value(url, "ack").as_deref().and_then(AckId::parse) {
                write_ack_logged(&ack, &AckRecord::failure(reason, None, None));
            }
            return;
        }
    };
    if request.invalid_ack {
        log::warn!("Ward: the new_tab URI has an invalid ack id; no ack file will be written");
    }
    if !request.path.is_dir() {
        log::warn!("Ward: the new_tab URI path is not a directory");
        if let Some(ack) = &request.ack {
            write_ack_logged(
                ack,
                &AckRecord::failure("path is not a directory", None, None),
            );
        }
        return;
    }

    // The token is checked only when there is a command to run.
    let mut rejection = None;
    let command = match request.command {
        Some(command) => {
            let expected = ensure_token(&token_path())
                .inspect_err(|err| log::warn!("Ward: could not read the URI token file: {err}"))
                .ok();
            match check_token(request.token.as_deref(), expected.as_deref()) {
                TokenCheck::Valid => {
                    log::info!("Ward: new_tab URI with a command ({} bytes)", command.len());
                    Some(command)
                }
                check => {
                    log::warn!(
                        "Ward: not running the new_tab URI command ({} bytes): {}",
                        command.len(),
                        check.reason()
                    );
                    rejection = Some(check.reason());
                    None
                }
            }
        }
        None => None,
    };

    let shell = request.shell.as_deref().and_then(|name| {
        let shell = find_shell(name, ctx);
        if shell.is_none() {
            log::warn!("Ward: unknown shell in a new_tab URI; using the default shell");
        }
        shell
    });
    let env_vars = command
        .as_deref()
        .map(|command| crate::ward::claude_launch::env_vars_for_command(command, ctx))
        .unwrap_or_default();
    let options = NewTerminalOptions {
        shell,
        initial_directory: Some(request.path.clone()),
        env_vars,
        ..Default::default()
    };
    let has_command = command.is_some();
    let tab = UriTab {
        options,
        title: request.title,
        ack: request.ack.clone().filter(|_| has_command),
        command,
    };

    let (window_id, opened) = open_tab_in_window(primary_window_id, tab, ctx);
    ctx.windows().show_window_and_focus_app(window_id);

    let Some(ack) = request.ack else {
        return;
    };
    if has_command && opened.is_some() {
        // The workspace registered the ack; it is written once the command is sent.
        return;
    }
    let record = match (opened, rejection) {
        (None, _) => AckRecord::failure("could not open the tab", None, Some(window_id)),
        (Some(opened), Some(reason)) => {
            AckRecord::failure(reason, opened.pane_uuid, Some(window_id))
        }
        // Nothing to send: the tab is all there is to confirm.
        (Some(opened), None) => AckRecord::success(opened.pane_uuid, Some(window_id)),
    };
    write_ack_logged(&ack, &record);
}

/// Opens `tab` in the workspace of `window_id`, or in a new window when there is none, and
/// queues its command.
pub(crate) fn open_tab_in_window(
    window_id: Option<WindowId>,
    tab: UriTab,
    ctx: &mut AppContext,
) -> (WindowId, Option<OpenedUriTab>) {
    let workspace_in = |window_id: WindowId, ctx: &AppContext| {
        ctx.views_of_type::<Workspace>(window_id)
            .and_then(|workspaces| workspaces.into_iter().next())
    };
    let target = window_id.and_then(|id| Some((id, workspace_in(id, ctx)?)));
    match target {
        Some((window_id, workspace)) => {
            let opened = workspace.update(ctx, |workspace, ctx| {
                workspace.ward_open_uri_tab(tab, true, ctx)
            });
            (window_id, opened)
        }
        None => {
            // No window: one with this session as its only tab.
            let (window_id, _) = crate::root_view::open_new_with_workspace_source(
                crate::root_view::NewWorkspaceSource::Session {
                    options: Box::new(tab.options.clone()),
                    initial_team_uid: None,
                },
                ctx,
            );
            let opened = workspace_in(window_id, ctx).and_then(|workspace| {
                workspace.update(ctx, |workspace, ctx| {
                    workspace.ward_open_uri_tab(tab, false, ctx)
                })
            });
            (window_id, opened)
        }
    }
}

#[cfg(test)]
#[path = "uri_launch_tests.rs"]
mod tests;
