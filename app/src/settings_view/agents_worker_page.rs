//! Settings → Agents worker (Ward only): Ward as a worker of agents-service, its settings and the
//! panel with the connection, slots, agents and sessions of this machine. The worker lives in
//! `crate::ward::agents`; this page saves what is entered and renders the worker's snapshot.
//!
//! The connection key goes from its masked input straight to the secure storage: the page never
//! shows it, keeps it or logs it, and neither does it the worker token.

use warpui::elements::{
    ChildView, CrossAxisAlignment, Element, Empty, Flex, MouseStateHandle, ParentElement,
};
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::SettingsSection;
use super::ai_shared::{styles, update_editor_interaction_state};
use super::cli_agents_page::{AGENTS_MAX_SESSION_ROWS, render_ward_toggle};
use super::settings_page::{
    CONTENT_FONT_SIZE, MatchData, PageTitle, PageType, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget,
};
use crate::appearance::Appearance;
use crate::view_components::{SubmittableTextInput, SubmittableTextInputEvent};
use crate::ward::agents as ward_agents;

const PAGE_TITLE: &str = "Agents worker";

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

pub struct AgentsWorkerPageView {
    page: PageType<Self>,
    url_editor: ViewHandle<SubmittableTextInput>,
    key_editor: ViewHandle<SubmittableTextInput>,
    name_editor: ViewHandle<SubmittableTextInput>,
    root_editor: ViewHandle<SubmittableTextInput>,
    clone_search_editor: ViewHandle<SubmittableTextInput>,
    remove_key_mouse_state: MouseStateHandle,
    /// One per session row link (Remote Control).
    link_mouse_states: Vec<MouseStateHandle>,
}

impl AgentsWorkerPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let (url_editor, key_editor, name_editor, root_editor, clone_search_editor) =
            Self::create_editors(ctx);
        if ctx.has_singleton_model::<ward_agents::AgentsModel>() {
            // The status panel follows the worker's snapshot.
            ctx.subscribe_to_model(&ward_agents::AgentsModel::handle(ctx), |_, _, _, ctx| {
                ctx.notify()
            });
        }
        Self {
            page: PageType::new_uncategorized(
                vec![Box::new(AgentsWorkerWidget::default())],
                Some(PageTitle::new(PAGE_TITLE)),
            ),
            url_editor,
            key_editor,
            name_editor,
            root_editor,
            clone_search_editor,
            remove_key_mouse_state: Default::default(),
            link_mouse_states: (0..AGENTS_MAX_SESSION_ROWS)
                .map(|_| Default::default())
                .collect(),
        }
    }

    /// Address, key, worker name, root and clone search inputs of the worker. Each saves on
    /// Enter; the worker picks the change up within a few seconds.
    fn create_editors(
        ctx: &mut ViewContext<Self>,
    ) -> (
        ViewHandle<SubmittableTextInput>,
        ViewHandle<SubmittableTextInput>,
        ViewHandle<SubmittableTextInput>,
        ViewHandle<SubmittableTextInput>,
        ViewHandle<SubmittableTextInput>,
    ) {
        let url_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx).validate_on_edit(|url| {
                url.trim().is_empty() || ward_agents::client::normalize_base_url(url).is_some()
            });
            input.set_placeholder_text(ward_agents::DEFAULT_BASE_URL, ctx);
            input
        });
        let key_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new_with_password(true, ctx);
            input.set_placeholder_text("connection key (agk_...)", ctx);
            input
        });
        let name_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx);
            input.set_placeholder_text("worker name (empty: host name)", ctx);
            input
        });
        let root_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx);
            input.set_placeholder_text("repository root (empty: ~/agents-work)", ctx);
            input
        });
        let clone_search_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx);
            input.set_placeholder_text(
                "existing clones or folders with clones, separated by ';' (empty: ~/work and similar)",
                ctx,
            );
            input
        });
        for editor in [
            &url_editor,
            &key_editor,
            &name_editor,
            &root_editor,
            &clone_search_editor,
        ] {
            update_editor_interaction_state(editor.as_ref(ctx).editor().clone(), true, ctx);
        }
        ctx.subscribe_to_view(&url_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(url) => {
                ward_agents::set_base_url(url, ctx);
                ctx.notify();
            }
            SubmittableTextInputEvent::Escape => ctx.emit(AgentsWorkerPageEvent::FocusModal),
        });
        ctx.subscribe_to_view(&key_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(key) => {
                ward_agents::set_key(key, ctx);
                ctx.notify();
            }
            SubmittableTextInputEvent::Escape => ctx.emit(AgentsWorkerPageEvent::FocusModal),
        });
        ctx.subscribe_to_view(&name_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(name) => {
                ward_agents::set_worker_name(name, ctx);
                ctx.notify();
            }
            SubmittableTextInputEvent::Escape => ctx.emit(AgentsWorkerPageEvent::FocusModal),
        });
        ctx.subscribe_to_view(&root_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(root) => {
                ward_agents::set_root(root, ctx);
                ctx.notify();
            }
            SubmittableTextInputEvent::Escape => ctx.emit(AgentsWorkerPageEvent::FocusModal),
        });
        ctx.subscribe_to_view(&clone_search_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(value) => {
                ward_agents::set_clone_search(value, ctx);
                ctx.notify();
            }
            SubmittableTextInputEvent::Escape => ctx.emit(AgentsWorkerPageEvent::FocusModal),
        });
        (
            url_editor,
            key_editor,
            name_editor,
            root_editor,
            clone_search_editor,
        )
    }
}

impl Entity for AgentsWorkerPageView {
    type Event = AgentsWorkerPageEvent;
}

impl View for AgentsWorkerPageView {
    fn ui_name() -> &'static str {
        "AgentsWorkerPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

pub enum AgentsWorkerPageEvent {
    FocusModal,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AgentsWorkerPageAction {
    /// Turns "Take tasks" on or off.
    ToggleTakeTasks,
    /// Removes the connection key (and the worker token) from the secure storage.
    RemoveKey,
}

impl TypedActionView for AgentsWorkerPageView {
    type Action = AgentsWorkerPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            AgentsWorkerPageAction::ToggleTakeTasks => {
                let paused = ward_agents::is_paused(ctx);
                ward_agents::set_paused(!paused, ctx);
                ctx.notify();
            }
            AgentsWorkerPageAction::RemoveKey => {
                ward_agents::set_key("", ctx);
                ctx.notify();
            }
        }
    }
}

impl SettingsPageMeta for AgentsWorkerPageView {
    fn section() -> SettingsSection {
        SettingsSection::AgentsWorker
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        crate::features::is_fork_oss()
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<AgentsWorkerPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<AgentsWorkerPageView>) -> Self {
        SettingsPageViewHandle::AgentsWorker(view_handle)
    }
}

/// Ward (OSS build): Ward as a worker of agents-service, its settings and the panel with the
/// connection, slots, agents and sessions of this machine.
#[derive(Default)]
struct AgentsWorkerWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for AgentsWorkerWidget {
    type View = AgentsWorkerPageView;

    fn search_terms(&self) -> &str {
        "agents worker agents-service key sessions worktree take tasks pause codex claude ward \
         profile mcp skills proxy vault plan preparing orchestrator"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        crate::features::is_fork_oss()
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        if !self.should_render(app) {
            return Empty::new().finish();
        }
        let label = |text: String| {
            appearance
                .ui_builder()
                .span(text)
                .with_style(UiComponentStyles {
                    font_size: Some(CONTENT_FONT_SIZE),
                    ..Default::default()
                })
                .build()
                .finish()
        };
        let description = |text: String| {
            appearance
                .ui_builder()
                .paragraph(text)
                .with_style(UiComponentStyles {
                    font_size: Some(appearance.ui_font_size()),
                    font_color: Some(styles::description_font_color(true, app).into()),
                    margin: Some(Coords::default().top(2.).bottom(8.)),
                    ..Default::default()
                })
                .build()
                .finish()
        };

        let (base_url, url_source) = ward_agents::base_url(app);
        let (name, name_source) = ward_agents::worker_name(app);
        let (root, root_source) = ward_agents::root(app);
        let (clone_search, clone_search_source) = ward_agents::clone_search(app);
        let key_source = ward_agents::key_source(app);
        let key_status = match key_source {
            Some(ward_agents::Source::Saved) => "A key is saved in the secure storage.".to_owned(),
            Some(ward_agents::Source::Environment | ward_agents::Source::Default) => {
                format!("Using {} from the environment.", ward_agents::KEY_ENV_VAR)
            }
            None => "Not set: the worker is off. The key is issued on the Agents screen in \
                     Chimney and shown once."
                .to_owned(),
        };

        let mut column = Flex::column()
            .with_child(description(format!(
                "Ward registers as a worker of agents-service, takes tasks started from Chimney \
                 cards and runs Claude Code or Codex in its own tabs, one git worktree per \
                 session under the repository root. A task without a prompt is prepared first \
                 by an orchestrator session in the vault of the machine profile. Where nothing \
                 is saved, {}, {}, {}, {} and {} are used.",
                ward_agents::URL_ENV_VAR,
                ward_agents::KEY_ENV_VAR,
                ward_agents::NAME_ENV_VAR,
                ward_agents::ROOT_ENV_VAR,
                ward_agents::LOCAL_REPOS_ENV_VAR,
            )))
            .with_child(render_ward_toggle(
                "Take tasks",
                "Off: new assignments are refused; running sessions go on and are still reported.",
                !ward_agents::is_paused(app),
                self.toggle.clone(),
                AgentsWorkerPageAction::ToggleTakeTasks,
                appearance,
                app,
            ))
            .with_child(label(format!(
                "Service address: {base_url}{}",
                source_suffix(url_source, ward_agents::URL_ENV_VAR)
            )))
            .with_child(ChildView::new(&view.url_editor).finish())
            .with_child(label("Connection key".to_owned()))
            .with_child(description(key_status))
            .with_child(ChildView::new(&view.key_editor).finish());
        if key_source == Some(ward_agents::Source::Saved) {
            let button = appearance
                .ui_builder()
                .close_button(16., view.remove_key_mouse_state.clone())
                .build()
                .on_click(|ctx, _, _| {
                    ctx.dispatch_typed_action(AgentsWorkerPageAction::RemoveKey);
                })
                .finish();
            column.add_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(6.)
                    .with_child(label("Remove saved key".to_owned()))
                    .with_child(button)
                    .finish(),
            );
        }
        column.add_child(label(format!(
            "Worker name: {name}{}",
            source_suffix(name_source, ward_agents::NAME_ENV_VAR)
        )));
        column.add_child(ChildView::new(&view.name_editor).finish());
        column.add_child(label(format!(
            "Repository root: {}{}",
            root.display(),
            source_suffix(root_source, ward_agents::ROOT_ENV_VAR)
        )));
        column.add_child(ChildView::new(&view.root_editor).finish());
        let clone_search_text = if clone_search.is_empty() {
            "none".to_owned()
        } else {
            clone_search
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join("; ")
        };
        column.add_child(label(format!(
            "Existing clones: {clone_search_text}{}",
            source_suffix(clone_search_source, ward_agents::LOCAL_REPOS_ENV_VAR)
        )));
        column.add_child(description(format!(
            "Before cloning, Ward looks for a clone with the same origin here: each entry is a \
             clone or a folder searched {} levels down. Such a clone is only fetched, its \
             working copy and branch stay as they are; the session's worktree goes under the \
             repository root.",
            ward_agents::git::SEARCH_DEPTH
        )));
        column.add_child(ChildView::new(&view.clone_search_editor).finish());

        let Some(snapshot) = ward_agents::model(app).map(|model| model.snapshot()) else {
            return column.finish();
        };
        column.add_child(label("Status".to_owned()));
        let mut status = connection_text(&snapshot.connection);
        if let Some(worker_id) = &snapshot.worker_id {
            status.push_str(&format!(" Worker id {worker_id}."));
        }
        if snapshot.server_paused || snapshot.limits.paused {
            status.push_str(" Paused on the Agents screen.");
        }
        if let Some(at) = snapshot.last_heartbeat {
            status.push_str(&format!(
                " Last heartbeat {}.",
                at.with_timezone(&chrono::Local).format("%H:%M:%S")
            ));
        }
        column.add_child(description(status));
        let active = snapshot
            .sessions
            .iter()
            .filter(|session| {
                matches!(
                    session.status,
                    ward_agents::contract::SessionStatus::Assigned
                        | ward_agents::contract::SessionStatus::Preparing
                        | ward_agents::contract::SessionStatus::PlanReview
                        | ward_agents::contract::SessionStatus::Running
                        | ward_agents::contract::SessionStatus::Blocked
                )
            })
            .count();
        let mut load = format!("Slots: {active} of {}.", snapshot.limits.max_sessions);
        if let Some(resources) = &snapshot.resources {
            load.push_str(&format!(
                " CPU {:.0}%, free memory {:.1} GB, free disk {:.0} GB.",
                resources.cpu_percent,
                resources.ram_free_bytes as f64 / GIB,
                resources.disk_free_bytes as f64 / GIB
            ));
        }
        load.push_str(&format!(
            " Limits: CPU {}%, keep {} GB memory and {} GB disk free, usage up to {}%.",
            snapshot.limits.cpu_percent,
            snapshot.limits.ram_reserve_gb,
            snapshot.limits.disk_min_free_gb,
            snapshot.limits.usage_max_percent
        ));
        column.add_child(description(load));

        column.add_child(label("Machine profile".to_owned()));
        for line in profile_text(snapshot) {
            column.add_child(description(line));
        }

        if !snapshot.agents.is_empty() {
            column.add_child(label("Agents".to_owned()));
            for (provider, info) in &snapshot.agents {
                let text = if !info.installed {
                    format!("{provider}: not installed")
                } else {
                    let mut text = format!(
                        "{provider} {}: {}",
                        info.version.as_deref().unwrap_or("(version unknown)"),
                        if info.logged_in {
                            "logged in"
                        } else {
                            "not logged in"
                        }
                    );
                    if let Some(plan) = &info.plan {
                        text.push_str(&format!(", plan {plan}"));
                    }
                    let usage = usage_text(provider, &snapshot.usage);
                    if !usage.is_empty() {
                        text.push_str(&format!(" \u{b7} {usage}"));
                    }
                    text
                };
                column.add_child(description(text));
            }
        }

        column.add_child(label("Sessions on this machine".to_owned()));
        if snapshot.sessions.is_empty() {
            column.add_child(description("None yet.".to_owned()));
        }
        for (session, mouse_state) in snapshot.sessions.iter().zip(view.link_mouse_states.iter()) {
            let number = session
                .task_number
                .map(|number| format!("#{number} "))
                .unwrap_or_default();
            let mut text = format!(
                "{number}{} \u{2014} {} ({}), {}",
                session.title,
                session.status.as_str(),
                session.provider,
                session.branch
            );
            if let Some(message) = &session.message {
                text.push_str(&format!(". {message}"));
            }
            column.add_child(description(text));
            if let Some(plan) = &session.plan {
                column.add_child(description(format!("Plan: {plan}")));
            }
            if let Some(url) = &session.remote_control_url {
                column.add_child(
                    appearance
                        .ui_builder()
                        .link(
                            "Remote Control".to_owned(),
                            Some(url.clone()),
                            None,
                            mouse_state.clone(),
                        )
                        .soft_wrap(false)
                        .build()
                        .with_margin_bottom(8.)
                        .finish(),
                );
            }
        }
        column.finish()
    }
}

fn source_suffix(source: ward_agents::Source, env_var: &str) -> String {
    match source {
        ward_agents::Source::Saved => " (saved)".to_owned(),
        ward_agents::Source::Environment => format!(" (from {env_var})"),
        ward_agents::Source::Default => " (default)".to_owned(),
    }
}

fn connection_text(connection: &ward_agents::worker::Connection) -> String {
    use ward_agents::worker::Connection;
    match connection {
        Connection::Disabled => "Off: no connection key.".to_owned(),
        Connection::Connecting => "Connecting\u{2026}".to_owned(),
        Connection::Online => "Online.".to_owned(),
        Connection::KeyRejected(reason) => {
            format!("The key was rejected ({reason}). Enter a new key from the Agents screen.")
        }
        Connection::Offline(reason) => format!("Service unreachable, retrying: {reason}."),
    }
}

/// What the panel says about the machine profile: the version, whether it is applied, the
/// proxy and the part that failed.
fn profile_text(snapshot: &ward_agents::worker::Snapshot) -> Vec<String> {
    let mut lines = Vec::new();
    match &snapshot.profile {
        None if snapshot.profile_version == 0 => {
            lines.push("No machine profile on the service yet.".to_owned());
        }
        None => lines.push(format!(
            "Version {} is not applied yet.",
            snapshot.profile_version
        )),
        Some(profile) => {
            let mut text = format!(
                "Version {} {}.",
                profile.version,
                if profile.ok {
                    "applied"
                } else {
                    "applied with errors"
                }
            );
            if snapshot.profile_version > profile.version {
                text.push_str(&format!(
                    " The service has {} already.",
                    snapshot.profile_version
                ));
            }
            if let Some(at) = profile.at {
                text.push_str(&format!(
                    " At {}.",
                    at.with_timezone(&chrono::Local).format("%H:%M:%S")
                ));
            }
            if !profile.message.is_empty() {
                text.push_str(&format!(" {}", profile.message));
            }
            lines.push(text);
            let proxy = match (&profile.proxy, profile.proxy_required) {
                (Some(proxy), true) => format!(
                    "Proxy: {} ({}).",
                    if proxy.ok { "passes" } else { "does not pass" },
                    proxy.message
                ),
                (_, false) => "Proxy: not required by the profile.".to_owned(),
                (None, true) => "Proxy: not checked yet.".to_owned(),
            };
            lines.push(proxy);
            for part in &profile.parts {
                lines.push(format!(
                    "{}: {} \u{2014} {}",
                    part.name,
                    if part.ok { "ok" } else { "failed" },
                    part.message
                ));
            }
        }
    }
    match &snapshot.vault {
        Some(vault) => lines.push(format!(
            "Vault: {vault}. Assignments without a prompt are prepared by an orchestrator \
             session there."
        )),
        None => lines
            .push("No vault in the profile: assignments without a prompt are refused.".to_owned()),
    }
    lines
}

/// `5h 12% · week 40%` from the usage reports of one provider.
fn usage_text(provider: &str, usage: &[ward_agents::contract::UsageReport]) -> String {
    let windows: Vec<String> = usage
        .iter()
        .filter(|report| report.provider == provider)
        .map(|report| match report.used_percent {
            Some(percent) => format!("{} {percent:.0}%", report.window),
            None => format!("{} ?", report.window),
        })
        .collect();
    windows.join(" \u{b7} ")
}
