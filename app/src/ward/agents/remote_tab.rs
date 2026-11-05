//! The tab of a session on another machine ([`super::attach`]) as Ward shows it: a tab of its
//! own that cannot pass for a local one.
//!
//! - The tab goes into the group "Удалённое управление", its name starts with the machine and
//!   its tab carries the remote icon; in the vertical tabs its row says which machine it is.
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

/// One tab of a session on another machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTab {
    /// The machine of the session; empty when the page did not say.
    pub worker: String,
    /// The task, `#4726 …`.
    pub task: String,
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
    pub fn new(worker: impl Into<String>, task: impl Into<String>) -> Self {
        Self {
            worker: worker.into(),
            task: task.into(),
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

    /// The line of the pane header.
    pub fn header(&self, phase: Phase) -> String {
        match phase {
            Phase::Starting | Phase::Live => {
                format!("Удалённое управление: {} · {}", self.machine(), self.task)
            }
            Phase::Ended => format!("Отключено от {} · {}", self.machine(), self.task),
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

    /// The line of the row in the vertical tabs.
    pub fn row_label(&self, phase: Phase) -> String {
        match phase {
            Phase::Starting | Phase::Live => format!("Удалённое управление · {}", self.machine()),
            Phase::Ended => format!("Отключено · {}", self.machine()),
        }
    }

    /// The tooltip of the tab.
    pub fn tooltip(&self) -> String {
        format!("Удалённое управление: {} · {}", self.machine(), self.task)
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
