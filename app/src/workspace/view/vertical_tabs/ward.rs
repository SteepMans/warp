//! Ward (OSS build) additions to the vertical tabs panel: the agent state line in terminal rows.
//! Kept out of `vertical_tabs.rs` so the upstream file only carries small hooks.

use warp_core::ui::color::coloru_with_opacity;
use warp_core::ui::theme::Fill as WarpThemeFill;
use warp_core::ui::theme::color::internal_colors;
use warpui::elements::{
    ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, DispatchEventResult, Element,
    EventHandler, Flex, MainAxisSize, ParentElement, Radius, Shrinkable, Text,
};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, EntityId, SingletonEntity};

use crate::workspace::Workspace;
use crate::appearance::Appearance;
use crate::pane_group::PaneId;
use crate::ward::agent_state::{AgentState, agent_snapshot, format_in_state};
use crate::ward::attention::{self, AgentStatusFilter, AttentionCounts};
use crate::ward::project_groups::remote_control_session_name;
use crate::workspace::action::WorkspaceAction;

const STATE_GLYPH_SIZE: f32 = 12.;
const COUNTER_GLYPH_SIZE: f32 = 10.;

/// Row tint for agents that need the user (waiting or failed). `None` keeps the default row.
pub(super) fn row_highlight_color(
    terminal_view_id: EntityId,
    app: &AppContext,
) -> Option<WarpThemeFill> {
    let snapshot = agent_snapshot(terminal_view_id, app)?;
    snapshot
        .state
        .needs_attention()
        .then(|| snapshot.state.color(Appearance::as_ref(app).theme()).into())
}

/// The agent line under a terminal row's title: state glyph, detail (question, tool and
/// argument, error) and time in the current state.
pub(super) fn render_agent_status_line(
    terminal_view_id: EntityId,
    appearance: &Appearance,
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    let snapshot = agent_snapshot(terminal_view_id, app)?;
    let theme = appearance.theme();
    let state_color = snapshot.state.color(theme);
    let sub_text = theme.sub_text_color(theme.background());
    // Only states that need the user color their text; the rest stay quiet.
    let detail_color: WarpThemeFill = if snapshot.state.needs_attention() {
        state_color.into()
    } else if snapshot.state == AgentState::Idle {
        theme.hint_text_color(theme.background())
    } else {
        sub_text
    };
    let font_family = appearance.ui_font_family();

    let glyph = ConstrainedBox::new(
        snapshot
            .state
            .icon()
            .to_warpui_icon(state_color.into())
            .finish(),
    )
    .with_width(STATE_GLYPH_SIZE)
    .with_height(STATE_GLYPH_SIZE)
    .finish();
    let detail = snapshot
        .detail
        .clone()
        .unwrap_or_else(|| snapshot.state.label().to_owned());
    let detail = Text::new_inline(detail, font_family, 12.)
        .with_clip(ClipConfig::ellipsis())
        .with_color(detail_color.into())
        .finish();
    let elapsed = Text::new_inline(format_in_state(snapshot.in_state), font_family, 11.)
        .with_color(sub_text.into())
        .finish();

    Some(
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.)
            .with_child(glyph)
            .with_child(Shrinkable::new(1., detail).finish())
            .with_child(Container::new(elapsed).with_margin_left(4.).finish())
            .finish(),
    )
}

/// Agent counts over the tabs of a group, for its header.
pub(super) fn group_agent_counts(
    workspace: &Workspace,
    members: &[(usize, Option<Vec<PaneId>>)],
    app: &AppContext,
) -> AttentionCounts {
    if !crate::features::is_fork_oss() {
        return AttentionCounts::default();
    }
    AttentionCounts::from_states(
        members
            .iter()
            .filter_map(|(tab_index, _)| workspace.tabs.get(*tab_index))
            .flat_map(|tab| tab.pane_group.as_ref(app).terminal_views(app))
            .filter_map(|terminal_view| agent_snapshot(terminal_view.id(), app))
            .map(|snapshot| snapshot.state),
    )
}

/// Appends waiting / error / running counters to a group header's subtitle. They stay visible
/// when the group is collapsed, so a folded project still shows that an agent needs the user.
pub(super) fn with_group_agent_counts(
    subtitle: Box<dyn Element>,
    counts: AttentionCounts,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let counters = [
        (counts.waiting, AgentState::WaitingAnswer),
        (counts.errors, AgentState::Error),
        (counts.running, AgentState::RunningTool),
    ];
    if counters.iter().all(|(count, _)| *count == 0) {
        return subtitle;
    }
    let mut row = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(subtitle);
    for (count, state) in counters {
        if count == 0 {
            continue;
        }
        let color = state.color(theme);
        row.add_child(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(2.)
                .with_child(
                    ConstrainedBox::new(state.icon().to_warpui_icon(color.into()).finish())
                        .with_width(COUNTER_GLYPH_SIZE)
                        .with_height(COUNTER_GLYPH_SIZE)
                        .finish(),
                )
                .with_child(
                    Text::new_inline(count.to_string(), appearance.ui_font_family(), 10.)
                        .with_color(color)
                        .finish(),
                )
                .finish(),
        );
    }
    row.finish()
}

/// Agent states in this window, for the filter chip counts.
fn workspace_agent_states(workspace: &Workspace, app: &AppContext) -> Vec<AgentState> {
    workspace
        .tabs
        .iter()
        .flat_map(|tab| tab.pane_group.as_ref(app).terminal_views(app))
        .filter_map(|terminal_view| agent_snapshot(terminal_view.id(), app))
        .map(|snapshot| snapshot.state)
        .collect()
}

/// Adds the All / Waiting / Running / Errors chips under the control bar while any agent runs
/// in this window (or a filter other than All is active, so it can be cleared).
pub(super) fn with_status_filter_chips(
    control_bar: Box<dyn Element>,
    workspace: &Workspace,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    if !crate::features::is_fork_oss() {
        return control_bar;
    }
    let states = workspace_agent_states(workspace, app);
    let active = attention::status_filter();
    if states.is_empty() && active == AgentStatusFilter::All {
        return control_bar;
    }
    let theme = appearance.theme();
    let font_family = appearance.ui_font_family();
    let main_text = theme.main_text_color(theme.background()).into_solid();
    let sub_text = theme.sub_text_color(theme.background()).into_solid();

    let mut chips = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(4.);
    for filter in AgentStatusFilter::ALL {
        let count_color = match filter {
            AgentStatusFilter::All => sub_text,
            AgentStatusFilter::Waiting => AgentState::WaitingAnswer.color(theme),
            AgentStatusFilter::Running => AgentState::RunningTool.color(theme),
            AgentStatusFilter::Errors => AgentState::Error.color(theme),
        };
        let is_active = filter == active;
        let chip = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(3.)
            .with_child(
                Text::new_inline(filter.label().to_owned(), font_family, 11.)
                    .with_color(if is_active { main_text } else { sub_text })
                    .finish(),
            )
            .with_child(
                Text::new_inline(filter.count(&states).to_string(), font_family, 11.)
                    .with_color(count_color)
                    .finish(),
            )
            .finish();
        let background = if is_active {
            internal_colors::fg_overlay_3(theme).into_solid()
        } else {
            coloru_with_opacity(sub_text, 12)
        };
        chips.add_child(
            EventHandler::new(
                Container::new(chip)
                    .with_horizontal_padding(7.)
                    .with_vertical_padding(1.)
                    .with_background(background)
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
                    .finish(),
            )
            .on_left_mouse_down(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::WardSetAgentStatusFilter(filter));
                DispatchEventResult::StopPropagation
            })
            .finish(),
        );
    }

    Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Start)
        .with_child(control_bar)
        .with_child(
            Container::new(chips.finish())
                .with_padding_left(8.)
                .with_padding_right(8.)
                .with_padding_bottom(4.)
                .finish(),
        )
        .finish()
}

/// Drops tabs without an agent matching the active status filter.
pub(super) fn filter_tabs_by_agent_status(
    visible_tabs: Vec<(usize, Option<Vec<PaneId>>)>,
    workspace: &Workspace,
    app: &AppContext,
) -> Vec<(usize, Option<Vec<PaneId>>)> {
    let filter = attention::status_filter();
    if !crate::features::is_fork_oss() || filter == AgentStatusFilter::All {
        return visible_tabs;
    }
    visible_tabs
        .into_iter()
        .filter(|(tab_index, _)| {
            workspace.tabs.get(*tab_index).is_some_and(|tab| {
                tab.pane_group
                    .as_ref(app)
                    .terminal_views(app)
                    .iter()
                    .filter_map(|terminal_view| agent_snapshot(terminal_view.id(), app))
                    .any(|snapshot| filter.matches(snapshot.state))
            })
        })
        .collect()
}

/// Extra search text for a terminal row: the agent's task, its question or other detail, and
/// the remote-control session name.
pub(super) fn agent_search_fragments(
    terminal_view: &crate::terminal::TerminalView,
    app: &AppContext,
) -> Vec<String> {
    let mut fragments = Vec::new();
    if let Some(snapshot) = agent_snapshot(terminal_view.id(), app) {
        fragments.extend(snapshot.task);
        fragments.extend(snapshot.detail);
    }
    if crate::features::is_fork_oss()
        && let Some(name) = terminal_view
            .claude_resume_info()
            .and_then(|info| remote_control_session_name(&info.command))
    {
        fragments.push(name);
    }
    fragments
}
