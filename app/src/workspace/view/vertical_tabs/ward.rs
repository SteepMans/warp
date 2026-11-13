//! Ward (OSS build) additions to the vertical tabs panel: the agent state line in terminal rows
//! and the line of a tab that controls a session on another machine.
//! Kept out of `vertical_tabs.rs` so the upstream file only carries small hooks.

use warp_core::ui::color::coloru_with_opacity;
use warp_core::ui::theme::Fill as WarpThemeFill;
use warp_core::ui::theme::color::internal_colors;
use warpui::color::ColorU;
use warpui::elements::{
    ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, DispatchEventResult, Element,
    EventHandler, Flex, MainAxisSize, ParentElement, Radius, Shrinkable, Text,
};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, EntityId, SingletonEntity};

use crate::appearance::Appearance;
use crate::features::FeatureFlag;
use crate::pane_group::PaneId;
use crate::terminal::TerminalView;
use crate::ui_components::icons::Icon;
use crate::ward::agent_state::{AgentState, agent_snapshot, format_in_state};
use crate::ward::agents::remote_tab::Link;
use crate::ward::attention::{self, AgentStatusFilter, AttentionCounts};
use crate::ward::project_groups::remote_control_session_name;
use crate::ward::tab_activity;
use crate::workspace::Workspace;
use crate::workspace::action::WorkspaceAction;
use crate::workspace::tab_settings::VerticalTabsDisplayGranularity;

const STATE_GLYPH_SIZE: f32 = 12.;
const COUNTER_GLYPH_SIZE: f32 = 10.;

/// Accent of a row whose agent needs the user (waiting or failed): the state color, full for
/// the row border and tinted for its background. `None` keeps the default row.
pub(super) fn row_attention_color(terminal_view_id: EntityId, app: &AppContext) -> Option<ColorU> {
    let snapshot = agent_snapshot(terminal_view_id, app)?;
    snapshot
        .state
        .needs_attention()
        .then(|| snapshot.state.color(Appearance::as_ref(app).theme()))
}

/// What the agent of a terminal row is doing, for the trailing indicators of the row title:
/// animated dots while it works, the state glyph while it waits for the user or has failed.
pub(super) fn render_agent_activity(
    terminal_view_id: EntityId,
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    tab_activity::render_tab_activity(terminal_view_id, STATE_GLYPH_SIZE, app)
}

/// Keeps the title line of a pinned row clear of the pin drawn over its top-right corner, so the
/// trailing indicators (agent activity, unread dot) are not hidden under it. Rows are padded by
/// [`ROW_PADDING`]; the pin ends [`super::PIN_INDICATOR_CORNER_INSET`] plus its size from the
/// edge.
pub(super) fn with_pin_clearance(
    title_line: Box<dyn Element>,
    is_pinned: bool,
) -> Box<dyn Element> {
    if !(crate::features::is_fork_oss() && FeatureFlag::PinnedTabs.is_enabled() && is_pinned) {
        return title_line;
    }
    Container::new(title_line)
        .with_padding_right(pin_clearance())
        .finish()
}

/// Inner padding of a vertical tab row (`Padding::uniform(8.)` in every row layout).
const ROW_PADDING: f32 = 8.;

/// Gap between the last indicator and the pin.
const PIN_GAP: f32 = 2.;

fn pin_clearance() -> f32 {
    (super::PIN_INDICATOR_CORNER_INSET + super::PIN_INDICATOR_ICON_SIZE + PIN_GAP - ROW_PADDING)
        .max(0.)
}

/// Background of an attention row. A plain tab color tint (15%) mixes with the group color
/// behind the row into grey on dark themes, so the state color is stronger here; the light
/// themes use a dark amber, which stays readable under the main text at these strengths.
pub(super) fn attention_row_background(color: ColorU, is_selected_or_hovered: bool) -> ColorU {
    coloru_with_opacity(color, if is_selected_or_hovered { 40 } else { 26 })
}

/// Whether a tab's own title (a rename or the `title=` of a `new_tab` URI) is the primary text
/// of its row in Panes view. Upstream drops it inside a tab group and shows it only as a header
/// above the row of an ungrouped tab; Ward puts it on the row in both layouts, so a tab placed
/// into a project group keeps its name. Only a single-pane tab: with several panes each row
/// keeps its own title and the header stays.
pub(super) fn tab_title_in_row(
    is_fork_oss: bool,
    display_granularity: VerticalTabsDisplayGranularity,
    visible_pane_count: usize,
) -> bool {
    is_fork_oss
        && matches!(display_granularity, VerticalTabsDisplayGranularity::Panes)
        && visible_pane_count == 1
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

/// The line under the name of a tab that controls a session on another machine: the remote
/// icon in the color of the stream, the machine and the branch of the session, or where the
/// stream is when there is no branch (`RemoteTab::row_label`). A compact row shows it in place
/// of the working directory and the branch of this machine, which say nothing of the session.
/// `font_size` is the one of the line it goes to.
pub(super) fn render_remote_line(
    terminal_view: &TerminalView,
    font_size: f32,
    appearance: &Appearance,
) -> Option<Box<dyn Element>> {
    let (tab, link) = terminal_view.ward_remote_link()?;
    let theme = appearance.theme();
    let text_color = if link == Link::Ended {
        theme.hint_text_color(theme.background())
    } else {
        theme.sub_text_color(theme.background())
    };
    let glyph = ConstrainedBox::new(
        Icon::Laptop
            .to_warpui_icon(remote_link_color(link, appearance).into())
            .finish(),
    )
    .with_width(font_size)
    .with_height(font_size)
    .finish();
    let label = Text::new_inline(tab.row_label(link), appearance.ui_font_family(), font_size)
        .with_clip(ClipConfig::ellipsis())
        .with_color(text_color.into())
        .finish();

    Some(
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.)
            .with_child(glyph)
            .with_child(Shrinkable::new(1., label).finish())
            .finish(),
    )
}

/// The color of the remote icon in the row: the remote color while the stream is live, the
/// same dimmed while it connects or waits for the Ward of the session, the warning color when
/// that Ward is old or gone, grey once the viewer is over.
fn remote_link_color(link: Link, appearance: &Appearance) -> ColorU {
    let theme = appearance.theme();
    match link {
        Link::Live => theme.ansi_fg_magenta(),
        Link::Connecting | Link::Waiting => coloru_with_opacity(theme.ansi_fg_magenta(), 50),
        Link::Legacy | Link::Reconnecting => theme.ui_warning_color(),
        Link::Ended => theme.sub_text_color(theme.background()).into_solid(),
    }
}

/// The hover of a remote tab, whose name is the task alone: the machine and the task in full,
/// and the branch of the session.
pub(super) fn render_remote_detail(
    terminal_view: &TerminalView,
    color: WarpThemeFill,
    appearance: &Appearance,
) -> Option<Box<dyn Element>> {
    let (tab, _) = terminal_view.ward_remote_tab()?;
    let mut detail = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Start)
        .with_spacing(4.)
        .with_child(super::render_detail_wrapping_text(
            tab.full_title(),
            12.,
            color,
            None,
            appearance,
        ));
    if !tab.branch.is_empty() {
        detail.add_child(super::render_git_branch_text(
            &tab.branch,
            color,
            12.,
            appearance,
        ));
    }
    Some(detail.finish())
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

/// Extra search text for a terminal row: the agent's task, its question or other detail, the
/// remote-control session name, and the machine and the branch of a remote tab.
pub(super) fn agent_search_fragments(
    terminal_view: &crate::terminal::TerminalView,
    app: &AppContext,
) -> Vec<String> {
    let mut fragments = Vec::new();
    if let Some(snapshot) = agent_snapshot(terminal_view.id(), app) {
        fragments.extend(snapshot.task);
        fragments.extend(snapshot.detail);
    }
    if let Some((tab, _)) = terminal_view.ward_remote_tab() {
        fragments.extend(
            [tab.worker, tab.branch]
                .into_iter()
                .filter(|s| !s.is_empty()),
        );
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
