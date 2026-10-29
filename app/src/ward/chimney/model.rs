//! Fetches Chimney boards off the main thread: every [`POLL_INTERVAL`] for the boards of bound
//! tabs (only while any tab is bound), and on demand for the card pickers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use super::api::{Board, CardStatus, card_status, parse_board};
use super::bindings::{self, CardRef};
use super::{RequestConfig, request_config};

pub const POLL_INTERVAL: Duration = Duration::from_secs(60);

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Larger responses are not boards Ward can use.
const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    NotConfigured,
    /// 401 or 403: the token is missing, expired or revoked.
    Unauthorized,
    Status(u16),
    Network,
    Parse,
}

impl FetchError {
    pub fn message(&self) -> String {
        match self {
            Self::NotConfigured => "No Chimney access token".to_owned(),
            Self::Unauthorized => "Chimney rejected the access token".to_owned(),
            Self::Status(status) => format!("Chimney returned HTTP {status}"),
            Self::Network => "Chimney is unreachable".to_owned(),
            Self::Parse => "Unexpected response from Chimney".to_owned(),
        }
    }
}

pub enum ChimneyEvent {
    /// A board was fetched (or failed to); chips and pickers should re-render.
    BoardUpdated { board_id: u64 },
}

pub struct ChimneyModel {
    client: Arc<http_client::Client>,
    boards: HashMap<u64, Board>,
    errors: HashMap<u64, FetchError>,
    in_flight: HashSet<u64>,
    polling: bool,
}

impl Entity for ChimneyModel {
    type Event = ChimneyEvent;
}

impl SingletonEntity for ChimneyModel {}

/// Registers the model in the OSS build.
pub fn init(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || cfg!(target_family = "wasm") {
        return;
    }
    ctx.add_singleton_model(|_| ChimneyModel {
        // A plain client: no Warp headers, and the network log never sees the token.
        client: Arc::new(http_client::Client::new_external()),
        boards: HashMap::new(),
        errors: HashMap::new(),
        in_flight: HashSet::new(),
        polling: false,
    });
}

/// The model, when registered (OSS desktop builds).
pub fn model(app: &AppContext) -> Option<&ChimneyModel> {
    app.has_singleton_model::<ChimneyModel>()
        .then(|| ChimneyModel::as_ref(app))
}

/// Starts polling if a live tab is bound and polling is not running yet.
pub fn ensure_polling(app: &mut AppContext) {
    if !app.has_singleton_model::<ChimneyModel>() {
        return;
    }
    let handle = ChimneyModel::handle(app);
    handle.update(app, |model, ctx| model.start_polling(ctx));
}

/// Fetches a board now, e.g. when a picker opens.
pub fn refresh_board(board_id: u64, app: &mut AppContext) {
    if !app.has_singleton_model::<ChimneyModel>() {
        return;
    }
    let handle = ChimneyModel::handle(app);
    handle.update(app, |model, ctx| model.fetch(board_id, ctx));
}

impl ChimneyModel {
    pub fn board(&self, board_id: u64) -> Option<&Board> {
        self.boards.get(&board_id)
    }

    pub fn error(&self, board_id: u64) -> Option<&FetchError> {
        self.errors.get(&board_id)
    }

    pub fn is_loading(&self, board_id: u64) -> bool {
        self.in_flight.contains(&board_id)
    }

    pub fn card_status(&self, card: CardRef) -> Option<CardStatus> {
        card_status(self.boards.get(&card.board_id)?, card.task_id)
    }

    fn start_polling(&mut self, ctx: &mut ModelContext<Self>) {
        if self.polling || bindings::live_boards(ctx).is_empty() {
            return;
        }
        self.polling = true;
        self.poll(ctx);
    }

    fn poll(&mut self, ctx: &mut ModelContext<Self>) {
        let boards = bindings::live_boards(ctx);
        if boards.is_empty() {
            self.polling = false;
            return;
        }
        for board_id in boards {
            self.fetch(board_id, ctx);
        }
        ctx.spawn(
            async move {
                Timer::after(POLL_INTERVAL).await;
            },
            |me, _, ctx| me.poll(ctx),
        );
    }

    fn fetch(&mut self, board_id: u64, ctx: &mut ModelContext<Self>) {
        if self.in_flight.contains(&board_id) {
            return;
        }
        let Some(config) = request_config(ctx) else {
            self.errors.insert(board_id, FetchError::NotConfigured);
            ctx.emit(ChimneyEvent::BoardUpdated { board_id });
            ctx.notify();
            return;
        };
        self.in_flight.insert(board_id);
        let client = self.client.clone();
        ctx.spawn(
            async move { fetch_board(&client, &config, board_id).await },
            move |me, result, ctx| {
                me.in_flight.remove(&board_id);
                match result {
                    Ok(board) => {
                        me.errors.remove(&board_id);
                        me.boards.insert(board_id, board);
                    }
                    Err(err) => {
                        log::info!("Ward: fetching Chimney board {board_id} failed: {}", err.message());
                        me.errors.insert(board_id, err);
                    }
                }
                ctx.emit(ChimneyEvent::BoardUpdated { board_id });
                ctx.notify();
            },
        );
    }
}

/// `GET {base}/api/v1/task-boards/{id}`, parsed on the background thread.
async fn fetch_board(
    client: &http_client::Client,
    config: &RequestConfig,
    board_id: u64,
) -> Result<Board, FetchError> {
    let url = format!("{}/api/v1/task-boards/{board_id}", config.base_url);
    let response = client
        .get(url)
        .bearer_auth(&config.token)
        .header("Accept", "application/json")
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(|_| FetchError::Network)?;
    let status = response.status().as_u16();
    match status {
        200..=299 => {}
        401 | 403 => return Err(FetchError::Unauthorized),
        _ => return Err(FetchError::Status(status)),
    }
    let body = response.text().await.map_err(|_| FetchError::Network)?;
    if body.len() > MAX_RESPONSE_BYTES {
        return Err(FetchError::Parse);
    }
    parse_board(&body).map_err(|_| FetchError::Parse)
}
