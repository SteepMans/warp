//! Ward (OSS build) workspace hooks. `view.rs` only calls into these methods so the upstream file
//! stays easy to merge.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use warp_core::ui::color::coloru_with_opacity;
use warpui::color::ColorU;
use warpui::elements::{
    ChildAnchor, Container, CornerRadius, CrossAxisAlignment, DispatchEventResult, Element,
    EventHandler, Flex, MainAxisSize, MouseStateHandle, ParentAnchor, ParentElement, Radius, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::geometry::vector::vec2f;
use warpui::{AppContext, EntityId, SingletonEntity, ViewContext};

use super::Workspace;
use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::pane_group::PanesLayout;
use crate::tab::{SelectedTabColor, TabData};
use crate::terminal::cli_agent_sessions::{CLIAgentSessionsModel, CLIAgentSessionsModelEvent};
use crate::util::bindings::keybinding_name_to_display_string;
use crate::ward::agent_state::AgentState;
use crate::ward::attention::AttentionCounts;
use crate::ward::claude_usage::usage::{self as claude_usage_text, UsageLevel};
use crate::ward::codex_usage::rollout as codex_usage_text;
use crate::ward::project_groups::{self, TabObservation};
use crate::ward::uri_launch::{self, OpenedUriTab, UriTab};
use crate::ward::{agent_state, attention, chimney, claude_usage, codex_usage};
use crate::workspace::WorkspaceRegistry;
use crate::workspace::action::WorkspaceAction;
use crate::workspace::tab_group::TabGroup;

/// Learns project groups from, and auto-places tabs in, every window. Runs whenever the app
/// state is saved, which covers tab and group changes, working directory changes and newly
/// detected agent sessions.
pub(crate) fn ward_sync_project_groups_in_all_windows(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() || !FeatureFlag::GroupedTabs.is_enabled() {
        return;
    }
    let workspaces = WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx);
    let mut alive = HashSet::new();
    for (_, workspace) in &workspaces {
        workspace.update(ctx, |workspace, ctx| {
            alive.extend(workspace.tabs.iter().map(|tab| tab.pane_group.id()));
            workspace.ward_sync_project_groups(ctx);
        });
    }
    project_groups::retain_tabs(|id| alive.contains(&id));
}

/// Refreshes which pane each terminal view shows (Chimney bindings are keyed by pane UUID),
/// binds panes whose agent command carries a `kanban:<board>/<task>` marker and starts card
/// polling when a live tab is bound. Runs whenever the app state is saved.
pub(crate) fn ward_sync_chimney_bindings_in_all_windows(ctx: &mut AppContext) {
    if !crate::features::is_fork_oss() {
        return;
    }
    let mut views = HashMap::new();
    let mut markers = Vec::new();
    for (_, workspace) in WorkspaceRegistry::as_ref(ctx).all_workspaces(ctx) {
        for pane_group in workspace.as_ref(ctx).tab_views() {
            let pane_group = pane_group.as_ref(ctx);
            for terminal_view in pane_group.terminal_views(ctx) {
                let Some(uuid) = pane_group.terminal_pane_session_uuid(terminal_view.id(), ctx)
                else {
                    continue;
                };
                let uuid = chimney::bindings::uuid_hex(&uuid);
                if let Some(card) = terminal_view
                    .as_ref(ctx)
                    .claude_resume_info()
                    .and_then(|info| chimney::bindings::parse_marker(&info.command))
                {
                    markers.push((uuid.clone(), card));
                }
                views.insert(terminal_view.id(), uuid);
            }
        }
    }
    chimney::bindings::set_live_views(views);
    // A marker binds a pane once; a binding the user changed by hand is kept.
    for (uuid, card) in markers {
        if chimney::bindings::card_of_pane(&uuid, ctx).is_none() {
            chimney::bindings::bind(&uuid, card, ctx);
        }
    }
    chimney::model::ensure_polling(ctx);
}

impl Workspace {
    /// Opens the tab of a Ward `new_tab` URI and queues its command, which runs once the shell
    /// has bootstrapped. With `add_tab` false the window was just created for this session and
    /// its active tab is used.
    pub(crate) fn ward_open_uri_tab(
        &mut self,
        tab: UriTab,
        add_tab: bool,
        ctx: &mut ViewContext<Self>,
    ) -> Option<OpenedUriTab> {
        let UriTab {
            options,
            title,
            command,
            ack,
        } = tab;
        if add_tab {
            self.add_tab_with_pane_layout(
                PanesLayout::SingleTerminal(Box::new(options)),
                Arc::new(HashMap::new()),
                title,
                ctx,
            );
        } else if let Some(title) = title {
            self.active_tab_pane_group()
                .update(ctx, |pane_group, ctx| pane_group.set_title(&title, ctx));
        }
        let terminal_view = self.active_session_view(ctx)?;
        let terminal_view_id = terminal_view.id();
        let pane_uuid = self
            .active_tab_pane_group()
            .as_ref(ctx)
            .terminal_pane_session_uuid(terminal_view_id, ctx)
            .map(|uuid| chimney::bindings::uuid_hex(&uuid));
        if let Some(command) = command {
            if let Some(ack) = ack {
                uri_launch::expect_command_sent(
                    terminal_view_id,
                    ack,
                    pane_uuid.clone(),
                    ctx.window_id(),
                );
                ctx.spawn(
                    async {
                        warpui::r#async::Timer::after(uri_launch::ACK_TIMEOUT).await;
                    },
                    move |_, _, _| uri_launch::expire_pending_ack(terminal_view_id),
                );
            }
            terminal_view.update(ctx, |view, ctx| {
                view.set_pending_command_queue(vec![command], ctx);
            });
        }
        ctx.notify();
        Some(OpenedUriTab { pane_uuid })
    }

    /// Re-renders (tab row card chips) when a Chimney board was fetched.
    pub(super) fn ward_subscribe_to_chimney(ctx: &mut ViewContext<Self>) {
        if !ctx.has_singleton_model::<chimney::model::ChimneyModel>() {
            return;
        }
        ctx.subscribe_to_model(
            &chimney::model::ChimneyModel::handle(ctx),
            |me, _, event, ctx| me.ward_handle_chimney_event(event, ctx),
        );
    }

    /// Re-renders the header usage chip when a Claude usage poll finished.
    pub(super) fn ward_subscribe_to_claude_usage(ctx: &mut ViewContext<Self>) {
        if !ctx.has_singleton_model::<claude_usage::model::ClaudeUsageModel>() {
            return;
        }
        ctx.subscribe_to_model(
            &claude_usage::model::ClaudeUsageModel::handle(ctx),
            |_, _, _, ctx| ctx.notify(),
        );
    }

    /// Re-renders the header usage chip when the Codex rate limits were read again.
    pub(super) fn ward_subscribe_to_codex_usage(ctx: &mut ViewContext<Self>) {
        if !ctx.has_singleton_model::<codex_usage::model::CodexUsageModel>() {
            return;
        }
        ctx.subscribe_to_model(
            &codex_usage::model::CodexUsageModel::handle(ctx),
            |_, _, _, ctx| ctx.notify(),
        );
    }

    fn ward_handle_chimney_event(
        &mut self,
        event: &chimney::model::ChimneyEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        let chimney::model::ChimneyEvent::BoardUpdated { board_id } = event;
        if *board_id == chimney::board_id(ctx) {
            self.ward_refresh_chimney_picker(ctx);
        }
        ctx.notify();
    }

    /// Header pills with agent counts across all windows: "2 waiting · 1 error · 3 running ·
    /// 1 done", then the Claude subscription usage chip and the agent overview button. Only
    /// waiting and error are colored; clicking them jumps to the waiting agent.
    pub(super) fn ward_render_attention_strip(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        if !crate::features::is_fork_oss() {
            return None;
        }
        let counts = AttentionCounts::from_states(
            attention::all_agents(app)
                .into_iter()
                .map(|agent| agent.state),
        );
        let theme = appearance.theme();
        let quiet = theme.sub_text_color(theme.background()).into_solid();
        let jump_shortcut =
            keybinding_name_to_display_string("workspace:ward_jump_to_waiting_agent", app);
        let error_label = if counts.errors == 1 {
            "error"
        } else {
            "errors"
        };
        let pills = [
            (
                counts.waiting,
                "waiting",
                Some(AgentState::WaitingAnswer.color(theme)),
                jump_shortcut,
            ),
            (
                counts.errors,
                error_label,
                Some(AgentState::Error.color(theme)),
                None,
            ),
            (counts.running, "running", None, None),
            (counts.done, "done", None, None),
        ];

        let mut strip = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.);
        for (count, label, accent, shortcut) in pills {
            if count == 0 {
                continue;
            }
            strip.add_child(render_attention_pill(
                &format!("{count} {label}"),
                accent,
                quiet,
                shortcut.as_deref(),
                appearance,
            ));
        }
        if let Some(usage_chip) = render_claude_usage_chip(
            self.mouse_states.ward_claude_usage.clone(),
            quiet,
            appearance,
            app,
        ) {
            strip.add_child(usage_chip);
        }
        if let Some(usage_chip) = render_codex_usage_chip(
            self.mouse_states.ward_codex_usage.clone(),
            quiet,
            appearance,
            app,
        ) {
            strip.add_child(usage_chip);
        }
        strip.add_child(render_overview_button(
            crate::ward::overview::is_open(self.window_id),
            quiet,
            keybinding_name_to_display_string("workspace:ward_toggle_agent_overview", app),
            appearance,
        ));
        Some(
            Container::new(strip.finish())
                .with_margin_left(8.)
                .with_margin_right(8.)
                .finish(),
        )
    }

    /// Focuses the agent that has waited longest for the user, or the next one when already on
    /// a waiting agent.
    pub(super) fn ward_jump_to_waiting_agent(&mut self, ctx: &mut ViewContext<Self>) {
        if !crate::features::is_fork_oss() {
            return;
        }
        let current = self.active_terminal_id(ctx);
        if let Some(target) = attention::begin_jump(current, ctx) {
            self.ward_focus_terminal_view(target, ctx);
        }
    }

    /// Returns to the terminal the last jump series started from.
    pub(super) fn ward_jump_back(&mut self, ctx: &mut ViewContext<Self>) {
        if !crate::features::is_fork_oss() {
            return;
        }
        if let Some(target) = attention::take_jump_back_target() {
            self.ward_focus_terminal_view(target, ctx);
        }
    }

    fn ward_focus_terminal_view(
        &mut self,
        terminal_view_id: EntityId,
        ctx: &mut ViewContext<Self>,
    ) {
        if !self.focus_terminal_view_locally(terminal_view_id, ctx) {
            self.focus_terminal_view_in_other_window(terminal_view_id, ctx);
        }
    }

    /// Terminal views of the active tab, in layout order.
    fn ward_active_tab_terminal_ids(&self, app: &AppContext) -> Vec<EntityId> {
        self.tabs
            .get(self.active_tab_index)
            .map(|tab| {
                tab.pane_group
                    .as_ref(app)
                    .visible_terminal_views(app)
                    .iter()
                    .map(|view| view.id())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A completed agent counts as seen once its tab is the active tab.
    pub(super) fn ward_mark_active_tab_seen(&self, ctx: &mut ViewContext<Self>) {
        if !crate::features::is_fork_oss() {
            return;
        }
        // Moving to a tab by any means leaves the agent overview board.
        self.ward_close_overview();
        let mut changed = false;
        for terminal_view_id in self.ward_active_tab_terminal_ids(ctx) {
            changed |= agent_state::mark_seen(terminal_view_id);
        }
        if changed {
            ctx.notify();
        }
    }

    pub(super) fn ward_handle_cli_agent_sessions_event(
        &mut self,
        event: &CLIAgentSessionsModelEvent,
        ctx: &mut ViewContext<Self>,
    ) {
        if !crate::features::is_fork_oss() {
            return;
        }
        // The attention strip and group counters cover agents in every window.
        if matches!(
            event,
            CLIAgentSessionsModelEvent::Started { .. }
                | CLIAgentSessionsModelEvent::StatusChanged { .. }
                | CLIAgentSessionsModelEvent::Ended { .. }
        ) {
            ctx.notify();
        }
        if let CLIAgentSessionsModelEvent::StatusChanged {
            terminal_view_id, ..
        } = event
            && self
                .ward_active_tab_terminal_ids(ctx)
                .contains(terminal_view_id)
        {
            agent_state::mark_seen(*terminal_view_id);
        }
    }

    /// Project keys of a tab, from its agent pane if it has one, else its first terminal.
    /// Outside a git repo only agent panes fall back to the working directory: a plain shell
    /// wandering through directories would otherwise teach the map every folder it visits.
    fn ward_tab_project_keys(tab: &TabData, app: &AppContext) -> Vec<String> {
        let terminal_views = tab.pane_group.as_ref(app).visible_terminal_views(app);
        let sessions = CLIAgentSessionsModel::as_ref(app);
        let agent_view = terminal_views
            .iter()
            .find(|view| sessions.session(view.id()).is_some());
        let Some(terminal_view) = agent_view.or_else(|| terminal_views.first()) else {
            return Vec::new();
        };
        let terminal_view = terminal_view.as_ref(app);
        let remote_control_project = terminal_view
            .claude_resume_info()
            .and_then(|info| project_groups::remote_control_project(&info.command));
        let directory = terminal_view
            .current_local_repo_path()
            .map(|path| path.to_string_lossy().into_owned())
            .or_else(|| {
                agent_view
                    .is_some()
                    .then(|| terminal_view.pwd_if_local(app))
                    .flatten()
            });
        project_groups::project_keys(remote_control_project.as_deref(), directory.as_deref())
    }

    fn ward_sync_project_groups(&mut self, ctx: &mut ViewContext<Self>) {
        let observations: Vec<(EntityId, TabObservation)> = self
            .tabs
            .iter()
            .map(|tab| {
                let group_name = tab
                    .group_id
                    .and_then(|group_id| self.tab_groups.get(&group_id))
                    .and_then(|group| group.name.clone());
                (
                    tab.pane_group.id(),
                    TabObservation {
                        keys: Self::ward_tab_project_keys(tab, ctx),
                        group_id: tab.group_id,
                        group_name,
                    },
                )
            })
            .collect();
        let moves: Vec<(EntityId, String)> = observations
            .into_iter()
            .filter_map(|(pane_group_id, observation)| {
                project_groups::observe_tab(pane_group_id, observation, ctx)
                    .map(|group| (pane_group_id, group))
            })
            .collect();
        for (pane_group_id, group) in moves {
            self.ward_move_tab_to_named_group(pane_group_id, &group, ctx);
        }
    }

    /// Moves a tab to the end of the group with the given name, creating the group when no
    /// group has that name any more. Unlike the menu actions, this never moves focus.
    fn ward_move_tab_to_named_group(
        &mut self,
        pane_group_id: EntityId,
        group_name: &str,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(tab_index) = self
            .tabs
            .iter()
            .position(|tab| tab.pane_group.id() == pane_group_id)
        else {
            return;
        };
        let previous_group_id = self.tabs[tab_index].group_id;
        let existing_group_id = self
            .tab_groups
            .iter()
            .find(|(_, group)| {
                group
                    .name
                    .as_deref()
                    .is_some_and(|name| project_groups::same_group_name(name, group_name))
            })
            .map(|(group_id, _)| *group_id);
        if existing_group_id.is_some() && existing_group_id == previous_group_id {
            return;
        }

        let (group_id, target_index) = match existing_group_id {
            Some(group_id) => (
                group_id,
                self.index_after_group(group_id).unwrap_or(self.tabs.len()),
            ),
            None => {
                let mut group = TabGroup::new();
                group.name = Some(group_name.to_owned());
                group.color = SelectedTabColor::Color(project_groups::auto_group_color(group_name));
                let group_id = group.id;
                self.tab_groups.insert(group_id, group);
                // A new single-member group stays where the tab is, past its old group so that
                // group stays contiguous, and out of the pinned region.
                let natural_index = previous_group_id
                    .and_then(|group_id| self.index_after_group(group_id))
                    .unwrap_or(tab_index);
                (
                    group_id,
                    self.clamp_to_unpinned_region(&self.tabs, natural_index),
                )
            }
        };

        self.tabs[tab_index].group_id = Some(group_id);
        self.tabs[tab_index].pinned = false;
        self.move_tab_to_index(tab_index, target_index, ctx);
        if self
            .tabs
            .get(self.active_tab_index)
            .is_some_and(|tab| tab.pane_group.id() == pane_group_id)
        {
            self.expand_tab_group(group_id, ctx);
        }
        if let Some(previous_group_id) = previous_group_id {
            self.prune_empty_tab_group(previous_group_id, ctx);
        }
        ctx.dispatch_global_action("workspace:save_app", ());
        ctx.notify();
    }
}

/// `Claude 5h 2% · 7d 60%`: Claude subscription usage, grey below 50%, then the warning and
/// error colors of the attention pills. The tooltip has the reset times in local time. Hidden
/// when turned off in the settings or without a fresh report (no credentials, 401).
fn render_claude_usage_chip(
    mouse_state: MouseStateHandle,
    quiet: ColorU,
    appearance: &Appearance,
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    if !claude_usage::is_enabled_by_user(app) {
        return None;
    }
    let usage = claude_usage::model::model(app)?.usage()?;
    let text = claude_usage_text::chip_text(usage)?;
    let tooltip = format!(
        "Claude subscription usage
{}",
        claude_usage_text::tooltip_text(usage, chrono::Utc::now(), &chrono::Local)
    );
    Some(render_usage_chip(
        format!("Claude {text}"),
        claude_usage_text::usage_level(usage),
        tooltip,
        mouse_state,
        quiet,
        appearance,
    ))
}

/// `Codex 5h 10% · 7d 30%`: the Codex plan limits from the newest Codex rollout, colored like
/// the Claude chip. Hidden when turned off in the settings, when no rollout has rate limits, or
/// once every window has reset since Codex last reported.
fn render_codex_usage_chip(
    mouse_state: MouseStateHandle,
    quiet: ColorU,
    appearance: &Appearance,
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    if !codex_usage::is_enabled_by_user(app) {
        return None;
    }
    let now = chrono::Utc::now();
    let usage = codex_usage::model::model(app)?.usage(now)?;
    let text = codex_usage_text::chip_text(&usage)?;
    let tooltip = format!(
        "Codex plan usage
{}",
        codex_usage_text::tooltip_text(&usage, now, &chrono::Local)
    );
    Some(render_usage_chip(
        format!("Codex {text}"),
        codex_usage_text::usage_level(&usage),
        tooltip,
        mouse_state,
        quiet,
        appearance,
    ))
}

fn render_usage_chip(
    text: String,
    level: UsageLevel,
    tooltip: String,
    mouse_state: MouseStateHandle,
    quiet: ColorU,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let (color, opacity) = match level {
        UsageLevel::Normal => (quiet, 10),
        UsageLevel::Warning => (AgentState::WaitingAnswer.color(theme), 13),
        UsageLevel::Critical => (AgentState::Error.color(theme), 13),
    };
    let chip = Container::new(
        Text::new_inline(text, appearance.ui_font_family(), 12.)
            .with_color(color)
            .with_style(Properties::default().weight(Weight::Semibold))
            .finish(),
    )
    .with_horizontal_padding(8.)
    .with_vertical_padding(3.)
    .with_background(coloru_with_opacity(color, opacity))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
    .finish();
    // An overlay, like the header icon buttons: a plain positioned child is clipped by the
    // header and only a few pixels of the tooltip would show under the chip.
    appearance.ui_builder().overlay_tool_tip_on_element(
        tooltip,
        mouse_state,
        chip,
        ParentAnchor::BottomMiddle,
        ChildAnchor::TopMiddle,
        vec2f(0., 4.),
    )
}

/// Header button that shows or hides the agent overview board.
fn render_overview_button(
    is_open: bool,
    quiet: ColorU,
    shortcut: Option<String>,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let font_family = appearance.ui_font_family();
    let mut content = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(
            Text::new_inline("Overview", font_family, 12.)
                .with_color(quiet)
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        );
    if let Some(shortcut) = shortcut {
        content.add_child(
            Text::new_inline(shortcut, font_family, 10.)
                .with_color(quiet)
                .finish(),
        );
    }
    let button = Container::new(content.finish())
        .with_horizontal_padding(8.)
        .with_vertical_padding(3.)
        .with_background(coloru_with_opacity(quiet, if is_open { 24 } else { 10 }))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
        .finish();
    EventHandler::new(button)
        .on_left_mouse_down(|ctx, _, _| {
            ctx.dispatch_typed_action(WorkspaceAction::Ward(
                crate::ward::action::WardAction::ToggleOverview,
            ));
            DispatchEventResult::StopPropagation
        })
        .finish()
}

/// One attention pill. Accented pills (waiting, error) get a tinted background and jump to the
/// most urgent agent on click; the rest stay grey so they do not pull the eye.
fn render_attention_pill(
    text: &str,
    accent: Option<ColorU>,
    quiet: ColorU,
    shortcut: Option<&str>,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let font_family = appearance.ui_font_family();
    let text_color = accent.unwrap_or(quiet);
    let mut content = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(
            Text::new_inline(text.to_owned(), font_family, 12.)
                .with_color(text_color)
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        );
    if let Some(shortcut) = shortcut {
        content.add_child(
            Text::new_inline(shortcut.to_owned(), font_family, 10.)
                .with_color(quiet)
                .finish(),
        );
    }
    let background = coloru_with_opacity(
        accent.unwrap_or(quiet),
        if accent.is_some() { 13 } else { 10 },
    );
    let pill = Container::new(content.finish())
        .with_horizontal_padding(8.)
        .with_vertical_padding(3.)
        .with_background(background)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
        .finish();
    if accent.is_none() {
        return pill;
    }
    EventHandler::new(pill)
        .on_left_mouse_down(|ctx, _, _| {
            ctx.dispatch_typed_action(WorkspaceAction::WardJumpToWaitingAgent);
            DispatchEventResult::StopPropagation
        })
        .finish()
}
