//! The session screen of Ward: the sessions of every machine in a list, and one of them open
//! with its chat and a place to write into it.
//!
//! Everything goes through [`super::gateway`], the owner's own view of the service, so a
//! session that runs on another Ward reads and answers exactly like a local one — there is no
//! second path for "mine" and no need to be the machine that runs the agent. The chat is the
//! journal of the session, topped up from the byte offset the screen already has and read as
//! replies by [`super::provider::chat`]; a message goes out as a `message` command, which the
//! service hands to that session's worker, which types it into the tab of its agent.
//!
//! Polling runs only while a session is open ([`POLL_INTERVAL`]), so a closed screen costs
//! nothing.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::gateway::{self, Chunk, Summary};
use super::provider::{self, Said};
use crate::ward::chimney::auth::Auth;
use crate::ward::chimney::model::FetchError;
use crate::ward::chimney::{self, RequestConfig};

/// How often the chat of the open session is topped up.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

/// How often the list of sessions is fetched again while a session is open.
const LIST_EVERY: Duration = Duration::from_secs(30);

/// How many lines of chat the open session keeps, so a session that ran for hours does not
/// grow without end. The oldest go first.
const MAX_CHAT_LINES: usize = 500;

/// What the screen shows about the open session.
pub struct Open {
    pub summary: Summary,
    /// Bytes of the journal the screen already has: the next request asks only for the rest.
    offset: u64,
    chat: VecDeque<Said>,
    /// Why the last top-up failed; `None` while the chat is fresh.
    pub error: Option<FetchError>,
    /// A `message` is on its way.
    pub sending: bool,
    /// Why the last message did not go out.
    pub send_error: Option<FetchError>,
    /// The journal was read at least once, so an empty chat means an empty transcript and not
    /// a screen that has not loaded yet.
    pub loaded: bool,
}

impl Open {
    /// The chat, oldest line first.
    pub fn chat(&self) -> impl Iterator<Item = &Said> {
        self.chat.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.chat.is_empty()
    }

    /// Reads the new lines of a journal chunk into the chat. Only whole lines are taken, and
    /// the offset moves by exactly those bytes (plus whatever beginning the chunk had to drop),
    /// so the line a chunk ended in the middle of comes back whole with the next request
    /// instead of being parsed broken.
    fn absorb(&mut self, chunk: &Chunk) {
        let (lines, bytes) = gateway::whole_lines(&chunk.text);
        if bytes == 0 && chunk.skipped == 0 {
            return;
        }
        self.offset += chunk.skipped + bytes;
        let agent = self
            .summary
            .provider
            .as_deref()
            .and_then(provider::by_id)
            .unwrap_or_else(|| provider::all()[0]);
        for said in provider::chat(agent, lines) {
            self.chat.push_back(said);
        }
        while self.chat.len() > MAX_CHAT_LINES {
            self.chat.pop_front();
        }
    }
}

pub enum SessionsEvent {
    /// The list, the chat or the state of a message changed.
    Updated,
}

/// The sessions of the service and the one the screen is on.
pub struct SessionsModel {
    client: Arc<http_client::Client>,
    sessions: Vec<Summary>,
    /// Why the list could not be fetched.
    list_error: Option<FetchError>,
    listing: bool,
    open: Option<Open>,
    /// A top-up of the chat is in flight: the poll does not pile requests up.
    reading: bool,
    polling: bool,
}

impl Entity for SessionsModel {
    type Event = SessionsEvent;
}

impl SingletonEntity for SessionsModel {}

/// Registers the model in the OSS build.
pub fn init(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    ctx.add_singleton_model(|_| SessionsModel {
        // A plain client: no Warp headers, and the network log never sees the token.
        client: Arc::new(http_client::Client::new_external()),
        sessions: Vec::new(),
        list_error: None,
        listing: false,
        open: None,
        reading: false,
        polling: false,
    });
}

/// The model, when registered (OSS desktop builds).
pub fn model(app: &AppContext) -> Option<&SessionsModel> {
    app.has_singleton_model::<SessionsModel>()
        .then(|| SessionsModel::as_ref(app))
}

fn with_model(
    app: &mut AppContext,
    update: impl FnOnce(&mut SessionsModel, &mut ModelContext<SessionsModel>),
) {
    if !app.has_singleton_model::<SessionsModel>() {
        return;
    }
    let handle = SessionsModel::handle(app);
    handle.update(app, |model, ctx| update(model, ctx));
}

/// Fetches the list of sessions now: the screen calls this when it opens and on "refresh".
pub fn refresh(app: &mut AppContext) {
    with_model(app, |model, ctx| model.fetch_list(ctx));
}

/// Opens a session: its chat is read from the start and topped up every [`POLL_INTERVAL`].
pub fn open(session_id: &str, app: &mut AppContext) {
    let session_id = session_id.to_owned();
    with_model(app, move |model, ctx| model.set_open(&session_id, ctx));
}

/// Back to the list; polling stops with the next tick.
pub fn close(app: &mut AppContext) {
    with_model(app, |model, ctx| {
        model.open = None;
        ctx.emit(SessionsEvent::Updated);
        ctx.notify();
    });
}

/// Types `text` into the agent of the open session.
pub fn send(text: &str, app: &mut AppContext) {
    let text = text.trim().to_owned();
    if text.is_empty() {
        return;
    }
    with_model(app, move |model, ctx| model.send_message(text, ctx));
}

impl SessionsModel {
    pub fn sessions(&self) -> &[Summary] {
        &self.sessions
    }

    pub fn list_error(&self) -> Option<&FetchError> {
        self.list_error.as_ref()
    }

    pub fn is_listing(&self) -> bool {
        self.listing
    }

    pub fn open_session(&self) -> Option<&Open> {
        self.open.as_ref()
    }

    /// The Chimney address and credentials; without them the screen says so instead of
    /// retrying into nothing.
    fn config(&mut self, ctx: &mut ModelContext<Self>) -> Option<RequestConfig> {
        match chimney::request_config(ctx) {
            Some(config) => Some(config),
            None => {
                self.list_error = Some(FetchError::NotConfigured);
                ctx.emit(SessionsEvent::Updated);
                ctx.notify();
                None
            }
        }
    }

    /// The token state every Chimney request of the app shares, so the session screen does not
    /// sign in a second time.
    fn auth(ctx: &AppContext) -> Arc<Auth> {
        chimney::model::shared_auth(ctx).unwrap_or_default()
    }

    fn fetch_list(&mut self, ctx: &mut ModelContext<Self>) {
        if self.listing {
            return;
        }
        let Some(config) = self.config(ctx) else {
            return;
        };
        self.listing = true;
        let client = self.client.clone();
        let auth = Self::auth(ctx);
        ctx.spawn(
            async move { gateway::sessions(&client, &auth, &config, gateway::LIST_LIMIT).await },
            |me, result, ctx| {
                me.listing = false;
                match result {
                    Ok(sessions) => {
                        me.list_error = None;
                        // The open session keeps its own summary up to date with the list.
                        if let Some(open) = me.open.as_mut()
                            && let Some(fresh) = sessions
                                .iter()
                                .find(|session| session.id == open.summary.id)
                        {
                            open.summary = fresh.clone();
                        }
                        me.sessions = sessions;
                    }
                    Err(err) => {
                        log::info!(
                            "Ward agents: the list of sessions failed: {}",
                            err.message()
                        );
                        me.list_error = Some(err);
                    }
                }
                ctx.emit(SessionsEvent::Updated);
                ctx.notify();
            },
        );
    }

    fn set_open(&mut self, session_id: &str, ctx: &mut ModelContext<Self>) {
        if self
            .open
            .as_ref()
            .is_some_and(|open| open.summary.id == session_id)
        {
            return;
        }
        let summary = self
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .cloned()
            .unwrap_or_else(|| Summary {
                id: session_id.to_owned(),
                ..Summary::default()
            });
        self.open = Some(Open {
            summary,
            offset: 0,
            chat: VecDeque::new(),
            error: None,
            sending: false,
            send_error: None,
            loaded: false,
        });
        self.reading = false;
        ctx.emit(SessionsEvent::Updated);
        ctx.notify();
        self.start_polling(ctx);
    }

    fn start_polling(&mut self, ctx: &mut ModelContext<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        self.poll(Duration::ZERO, ctx);
    }

    /// Tops the chat up while a session is open, and refreshes the list every [`LIST_EVERY`].
    fn poll(&mut self, since_list: Duration, ctx: &mut ModelContext<Self>) {
        if self.open.is_none() {
            self.polling = false;
            return;
        }
        let since_list = if since_list >= LIST_EVERY {
            self.fetch_list(ctx);
            Duration::ZERO
        } else {
            since_list
        };
        self.read_journal(ctx);
        ctx.spawn(
            async move {
                Timer::after(POLL_INTERVAL).await;
            },
            move |me, _, ctx| me.poll(since_list + POLL_INTERVAL, ctx),
        );
    }

    fn read_journal(&mut self, ctx: &mut ModelContext<Self>) {
        if self.reading {
            return;
        }
        let Some((session_id, offset)) = self
            .open
            .as_ref()
            .map(|open| (open.summary.id.clone(), open.offset))
        else {
            return;
        };
        let Some(config) = self.config(ctx) else {
            return;
        };
        self.reading = true;
        let client = self.client.clone();
        let auth = Self::auth(ctx);
        let wanted = session_id.clone();
        ctx.spawn(
            async move { gateway::journal(&client, &auth, &config, &session_id, offset).await },
            move |me, result, ctx| {
                me.reading = false;
                let Some(open) = me.open.as_mut() else {
                    return;
                };
                // The screen moved on while the request was in flight: this chunk is not of
                // the session it shows now, and its bytes belong to no offset here.
                if open.summary.id != wanted || open.offset != offset {
                    return;
                }
                match result {
                    Ok(chunk) => {
                        open.error = None;
                        open.loaded = true;
                        open.absorb(&chunk);
                    }
                    Err(err) => {
                        log::info!(
                            "Ward agents: the chat of a session failed: {}",
                            err.message()
                        );
                        open.error = Some(err);
                    }
                }
                ctx.emit(SessionsEvent::Updated);
                ctx.notify();
            },
        );
    }

    fn send_message(&mut self, text: String, ctx: &mut ModelContext<Self>) {
        let Some(session_id) = self
            .open
            .as_ref()
            .filter(|open| !open.sending)
            .map(|open| open.summary.id.clone())
        else {
            return;
        };
        let Some(config) = self.config(ctx) else {
            return;
        };
        if let Some(open) = self.open.as_mut() {
            open.sending = true;
            open.send_error = None;
        }
        let client = self.client.clone();
        let auth = Self::auth(ctx);
        ctx.spawn(
            async move { gateway::send_message(&client, &auth, &config, &session_id, &text).await },
            |me, result, ctx| {
                if let Some(open) = me.open.as_mut() {
                    open.sending = false;
                    open.send_error = result.as_ref().err().cloned();
                }
                if let Err(err) = &result {
                    log::info!(
                        "Ward agents: a message for a session did not go out: {}",
                        err.message()
                    );
                }
                ctx.emit(SessionsEvent::Updated);
                ctx.notify();
            },
        );
    }
}

#[cfg(test)]
#[path = "sessions_tests.rs"]
mod tests;
