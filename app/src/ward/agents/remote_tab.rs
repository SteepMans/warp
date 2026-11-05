//! The tab of a session on another machine ([`super::attach`]) as Ward shows it: a tab of its
//! own that cannot pass for a local one.
//!
//! - The tab goes into the group "Удалённое управление" and carries the remote icon. Its name
//!   is the task, so a narrow row still reads it; the machine and the branch of the session go
//!   to the line under it ([`RemoteTab::row_label`]), the remote icon of that line takes the
//!   color of the stream ([`Link`]), and the hover names the machine and the task in full.
//! - The pane header stays on while the tab lives: "Удалённое управление: steep-pc · #4726 …",
//!   where the stream is and how to disconnect. It is outside the terminal, so the output of
//!   the session cannot overwrite it. Where the stream is, the viewer says in the title of its
//!   console ([`viewer_status`]).
//! - The command that runs the viewer and the input box are hidden while the viewer runs. Once
//!   it is over (disconnected, or the stream ended) the tab is a plain shell again and says so.
//!
//! Which terminals are such tabs lives here, on the main thread, by the id of the terminal view.

use std::cell::RefCell;
use std::collections::HashMap;

use warpui::EntityId;

/// The tab group of the remote tabs.
pub const GROUP_NAME: &str = "Удалённое управление";

/// How to leave the remote tab, as the viewer reads it.
pub const DETACH_HINT: &str = "Ctrl+] — отключиться";

/// How the viewer titles its console with the state of the stream: `Ward: подключено`, and
/// `Ward! …` for a warning (an older Ward there, the Ward gone and asked for again).
pub const STATUS_PREFIX: &str = "Ward: ";
pub const WARNING_PREFIX: &str = "Ward! ";
const MAX_STATUS_CHARS: usize = 160;

/// How the viewer words its states, as `attach.ps1` writes them: the stream is live, the
/// service waits for the Ward of the session, the stream is over, and the warning of an older
/// Ward there. Any other warning is the Ward of the session gone and asked for again.
pub(crate) const LIVE_TEXT: &str = "подключено";
pub(crate) const WAITING_TEXT: &str = "ждём Ward на ";
pub(crate) const ENDED_TEXT: &str = "трансляция закончилась";
pub(crate) const LEGACY_TEXT: &str = "Ward старой версии";

/// Where the stream of a remote tab is, as its viewer says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerStatus {
    pub text: String,
    pub warning: bool,
}

/// The state of the stream in the title the viewer gives its console; `None` for any other
/// title (the shell's, before the viewer runs).
pub fn viewer_status(title: &str) -> Option<ViewerStatus> {
    let (rest, warning) = if let Some(rest) = title.strip_prefix(STATUS_PREFIX) {
        (rest, false)
    } else {
        (title.strip_prefix(WARNING_PREFIX)?, true)
    };
    let text: String = rest
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_STATUS_CHARS)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| ViewerStatus {
        text: text.to_owned(),
        warning,
    })
}

/// Where the stream of a remote tab is, for its row in the tab list: the remote icon there
/// takes its color, and a row without a branch names it. The same states as the viewer has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    /// The viewer starts or opens the socket of the service.
    Connecting,
    /// The service waits for the Ward of the session: the session is starting.
    Waiting,
    /// The Ward of the session streams.
    Live,
    /// An older Ward streams: the screen may come out doubled.
    Legacy,
    /// The Ward of the session went away and is asked for again.
    Reconnecting,
    /// The viewer is over, or the stream ended.
    Ended,
}

impl Link {
    /// The state of the stream, given where the viewer is and what it titled its console with.
    pub fn of(phase: Phase, status: Option<&ViewerStatus>) -> Self {
        let status = match phase {
            Phase::Ended => return Link::Ended,
            Phase::Starting => return Link::Connecting,
            Phase::Live => match status {
                Some(status) => status,
                None => return Link::Connecting,
            },
        };
        if status.warning {
            if status.text.contains(LEGACY_TEXT) {
                Link::Legacy
            } else {
                Link::Reconnecting
            }
        } else if status.text == LIVE_TEXT {
            Link::Live
        } else if status.text == ENDED_TEXT {
            Link::Ended
        } else if status.text.starts_with(WAITING_TEXT) {
            Link::Waiting
        } else {
            Link::Connecting
        }
    }

    /// A word or two for the row of a tab without a branch.
    pub fn label(self) -> &'static str {
        match self {
            Link::Connecting => "подключение…",
            Link::Waiting => "ждём Ward",
            Link::Live => "подключено",
            Link::Legacy => "старый Ward",
            Link::Reconnecting => "переподключение…",
            Link::Ended => "отключено",
        }
    }
}

/// One tab of a session on another machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTab {
    /// The machine of the session; empty when the page did not say.
    pub worker: String,
    /// The task, `#4726 …`.
    pub task: String,
    /// The branch of the session; empty when the page did not say.
    pub branch: String,
    /// The block of the viewer's command, once the shell started it.
    block: Option<String>,
}

/// Where the viewer of a remote tab is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// The shell of the tab is starting; the viewer has not run yet.
    Starting,
    /// The viewer runs: the keys go to the session.
    Live,
    /// The viewer is over: the tab is a plain shell now.
    Ended,
}

impl RemoteTab {
    pub fn new(
        worker: impl Into<String>,
        task: impl Into<String>,
        branch: impl Into<String>,
    ) -> Self {
        Self {
            worker: worker.into(),
            task: task.into(),
            branch: branch.into(),
            block: None,
        }
    }

    /// The phase, given the active block of the terminal and whether it has finished.
    pub fn phase(&self, active_block: &str, active_block_finished: bool) -> Phase {
        match &self.block {
            None => Phase::Starting,
            Some(block) if block == active_block && !active_block_finished => Phase::Live,
            Some(_) => Phase::Ended,
        }
    }

    fn machine(&self) -> &str {
        if self.worker.is_empty() {
            "другая машина"
        } else {
            &self.worker
        }
    }

    /// The machine and the task in full, `steep-pc · #4726 …`: for the hover of the tab, whose
    /// name is the task alone.
    pub fn full_title(&self) -> String {
        format!("{} · {}", self.machine(), self.task)
    }

    /// The line of the pane header.
    pub fn header(&self, phase: Phase) -> String {
        match phase {
            Phase::Starting | Phase::Live => {
                format!("Удалённое управление: {}", self.full_title())
            }
            Phase::Ended => format!("Отключено от {}", self.full_title()),
        }
    }

    /// What the header says on the right: how to disconnect while connected.
    pub fn hint(phase: Phase) -> Option<&'static str> {
        match phase {
            Phase::Starting => Some("подключение…"),
            Phase::Live => Some(DETACH_HINT),
            Phase::Ended => None,
        }
    }

    /// The line under the name of the tab in the vertical tabs: the machine and the branch of
    /// the session, `steep-pc · feature/4726-fireworks-sync`; without a branch, the machine and
    /// where the stream is. Without a machine, the branch or the state alone: the remote icon in
    /// front of the line already says it is another machine.
    pub fn row_label(&self, link: Link) -> String {
        let detail = if self.branch.is_empty() {
            link.label()
        } else {
            self.branch.as_str()
        };
        if self.worker.is_empty() {
            detail.to_owned()
        } else {
            format!("{} · {detail}", self.worker)
        }
    }

    /// The tooltip of the tab.
    pub fn tooltip(&self) -> String {
        format!("Удалённое управление: {}", self.full_title())
    }
}

thread_local! {
    static TABS: RefCell<HashMap<EntityId, RemoteTab>> = RefCell::new(HashMap::new());
}

/// Marks the terminal view `view` as the tab of a session on another machine.
pub fn register(view: EntityId, tab: RemoteTab) {
    TABS.with(|tabs| {
        tabs.borrow_mut().insert(view, tab);
    });
}

/// Forgets the tabs whose terminal is gone.
pub fn retain(mut is_alive: impl FnMut(EntityId) -> bool) {
    TABS.with(|tabs| tabs.borrow_mut().retain(|view, _| is_alive(*view)));
}

/// The remote tab of the terminal view `view`, if it is one.
pub fn of_view(view: EntityId) -> Option<RemoteTab> {
    TABS.with(|tabs| tabs.borrow().get(&view).cloned())
}

pub fn is_remote(view: EntityId) -> bool {
    TABS.with(|tabs| tabs.borrow().contains_key(&view))
}

/// A command block started in the terminal view `view`. The first one of a remote tab is the
/// viewer's: true when this is it, so its command line is hidden.
pub fn claim_block(view: EntityId, block: &str) -> bool {
    TABS.with(|tabs| {
        let mut tabs = tabs.borrow_mut();
        match tabs.get_mut(&view) {
            Some(tab) if tab.block.is_none() => {
                tab.block = Some(block.to_owned());
                true
            }
            _ => false,
        }
    })
}

#[cfg(test)]
#[path = "remote_tab_tests.rs"]
mod tests;
