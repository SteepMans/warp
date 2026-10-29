//! Ward (OSS build) agent overview board, shown in place of the active tab while it is open in
//! the window. Built straight from the agent snapshots on every render; the only terminal data
//! read is the last rows of each agent's grid.

use warp_core::ui::color::coloru_with_opacity;
use warp_core::ui::theme::color::internal_colors;
use warpui::color::ColorU;
use warpui::elements::{
    Clipped, Container, CornerRadius, CrossAxisAlignment, DispatchEventResult, Element,
    EventHandler, Expanded, Flex, MainAxisSize, ParentElement, Radius, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, EntityId, SingletonEntity, ViewContext};

use super::Workspace;
use crate::appearance::Appearance;
use crate::ward::action::WardAction;
use crate::ward::agent_state::{self, AgentState, format_in_state};
use crate::ward::attention::{self, AgentEntry};
use crate::ward::overview::{self, OverviewColumn, card_order};
use crate::workspace::action::WorkspaceAction;

const CARD_SPACING: f32 = 6.;

impl Workspace {
    /// The board, when it is open in this window.
    pub(super) fn ward_render_overview(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        if !crate::features::is_fork_oss() || !overview::is_open(self.window_id) {
            return None;
        }
        let appearance = Appearance::as_ref(app);
        let mut entries = attention::all_agent_entries(app);
        entries.sort_by(|a, b| {
            card_order(
                (a.snapshot.state, a.snapshot.in_state),
                (b.snapshot.state, b.snapshot.in_state),
            )
        });

        let mut board = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(10.);
        for column in OverviewColumn::ALL {
            let cards: Vec<&AgentEntry> = entries
                .iter()
                .filter(|entry| OverviewColumn::of(entry.snapshot.state) == column)
                .collect();
            board.add_child(Expanded::new(1., render_column(column, &cards, appearance, app)).finish());
        }

        let theme = appearance.theme();
        let hint = if entries.is_empty() {
            "No CLI agents are running. Start Claude Code or Codex in a tab to see it here."
        } else {
            "Open focuses the agent's tab; the board closes on any tab switch."
        };
        let shortcut = crate::util::bindings::keybinding_name_to_display_string(
            "workspace:ward_toggle_agent_overview",
            app,
        )
        .map(|shortcut| format!("  {shortcut} toggles the board."))
        .unwrap_or_default();
        let header = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(8.)
            .with_child(
                Text::new_inline("Agent overview", appearance.ui_font_family(), 14.)
                    .with_color(theme.main_text_color(theme.background()).into_solid())
                    .with_style(Properties::default().weight(Weight::Semibold))
                    .finish(),
            )
            .with_child(
                Shrinkable::new(
                    1.,
                    Text::new_inline(format!("{hint}{shortcut}"), appearance.ui_font_family(), 11.)
                        .with_clip(ClipConfig::ellipsis())
                        .with_color(theme.sub_text_color(theme.background()).into_solid())
                        .finish(),
                )
                .finish(),
            )
            .finish();

        Some(
            Container::new(
                Flex::column()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                    .with_child(Container::new(header).with_margin_bottom(12.).finish())
                    .with_child(Expanded::new(1., Clipped::new(board.finish()).finish()).finish())
                    .finish(),
            )
            .with_uniform_padding(16.)
            .with_background(theme.background().into_solid())
            .finish(),
        )
    }

    pub(super) fn ward_handle_action(&mut self, action: &WardAction, ctx: &mut ViewContext<Self>) {
        if !crate::features::is_fork_oss() {
            return;
        }
        match action {
            WardAction::ToggleOverview => {
                let open = !overview::is_open(self.window_id);
                overview::set_open(self.window_id, open);
                if open {
                    // Keys must not reach the hidden terminal while the board covers it.
                    ctx.focus_self();
                } else {
                    self.focus_active_tab(ctx);
                }
                ctx.notify();
            }
            WardAction::OpenAgent(terminal_view_id) => {
                overview::set_open(self.window_id, false);
                self.ward_open_agent(*terminal_view_id, ctx);
                ctx.notify();
            }
            WardAction::MarkAgentSeen(terminal_view_id) => {
                if agent_state::mark_seen(*terminal_view_id) {
                    ctx.notify();
                }
            }
            WardAction::CloseAgentTab(terminal_view_id) => {
                let tab_index = self.tabs.iter().position(|tab| {
                    tab.pane_group
                        .as_ref(ctx)
                        .contains_terminal_view(*terminal_view_id, ctx)
                });
                match tab_index {
                    Some(index) => self.close_tab(index, false, true, ctx),
                    // Tabs of other windows are closed from their own window.
                    None => self.ward_open_agent(*terminal_view_id, ctx),
                }
                ctx.notify();
            }
        }
    }

    /// Focuses an agent's terminal in this or another window.
    pub(super) fn ward_open_agent(&mut self, terminal_view_id: EntityId, ctx: &mut ViewContext<Self>) {
        if !self.focus_terminal_view_locally(terminal_view_id, ctx) {
            self.focus_terminal_view_in_other_window(terminal_view_id, ctx);
        }
    }

    /// Closes the board when the user moves to a tab by any other means.
    pub(super) fn ward_close_overview(&self) {
        overview::set_open(self.window_id, false);
    }
}

fn render_column(
    column: OverviewColumn,
    cards: &[&AgentEntry],
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    let theme = appearance.theme();
    let heading = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(
            Text::new_inline(column.title(), appearance.ui_font_family(), 12.)
                .with_color(theme.main_text_color(theme.background()).into_solid())
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        )
        .with_child(
            Text::new_inline(cards.len().to_string(), appearance.ui_font_family(), 12.)
                .with_color(theme.sub_text_color(theme.background()).into_solid())
                .finish(),
        )
        .finish();
    let mut content = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_child(Container::new(heading).with_margin_bottom(8.).finish());
    for entry in cards {
        content.add_child(
            Container::new(render_card(entry, appearance, app))
                .with_margin_bottom(CARD_SPACING)
                .finish(),
        );
    }
    Container::new(content.finish())
        .with_uniform_padding(8.)
        .with_background(internal_colors::fg_overlay_1(theme).into_solid())
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
        .finish()
}

fn render_card(entry: &AgentEntry, appearance: &Appearance, app: &AppContext) -> Box<dyn Element> {
    let theme = appearance.theme();
    let snapshot = &entry.snapshot;
    let state_color = snapshot.state.color(theme);
    let main_text = theme.main_text_color(theme.background()).into_solid();
    let sub_text = theme.sub_text_color(theme.background()).into_solid();
    let font = appearance.ui_font_family();
    let terminal_view_id = entry.terminal_view.id();

    let top = Flex::row()
        .with_main_axis_size(MainAxisSize::Max)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(
            Shrinkable::new(
                1.,
                Text::new_inline(
                    entry.labels.project.clone().unwrap_or_default(),
                    font,
                    11.,
                )
                .with_clip(ClipConfig::ellipsis())
                .with_color(sub_text)
                .finish(),
            )
            .finish(),
        )
        .with_child(
            Text::new_inline(format_in_state(snapshot.in_state), font, 11.)
                .with_color(sub_text)
                .finish(),
        )
        .finish();

    let mut card = Flex::column()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
        .with_spacing(3.)
        .with_child(top)
        .with_child(
            Text::new_inline(entry.labels.task.clone(), font, 13.)
                .with_clip(ClipConfig::ellipsis())
                .with_color(main_text)
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish(),
        );

    let detail = snapshot
        .detail
        .clone()
        .unwrap_or_else(|| snapshot.state.label().to_owned());
    let detail_color = if snapshot.state.needs_attention() {
        state_color
    } else {
        sub_text
    };
    card.add_child(
        Text::new_inline(detail, font, 12.)
            .with_clip(ClipConfig::ellipsis())
            .with_color(detail_color)
            .finish(),
    );

    // The tail is worth reading where the agent waits for the user or failed.
    if snapshot.state.needs_attention() {
        let tail = overview::terminal_tail(entry.terminal_view.as_ref(app));
        if !tail.is_empty() {
            let mut tail_column = Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Start);
            for line in tail {
                tail_column.add_child(
                    Text::new_inline(line, appearance.monospace_font_family(), 11.)
                        .with_clip(ClipConfig::ellipsis())
                        .with_color(sub_text)
                        .finish(),
                );
            }
            card.add_child(
                Container::new(tail_column.finish())
                    .with_horizontal_padding(6.)
                    .with_vertical_padding(4.)
                    .with_background(internal_colors::fg_overlay_1(theme).into_solid())
                    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                    .finish(),
            );
        }
    }

    let mut actions = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(render_card_button(
            "Open",
            WardAction::OpenAgent(terminal_view_id),
            sub_text,
            appearance,
        ));
    match snapshot.state {
        AgentState::DoneUnseen => actions.add_child(render_card_button(
            "Mark seen",
            WardAction::MarkAgentSeen(terminal_view_id),
            sub_text,
            appearance,
        )),
        AgentState::Idle => actions.add_child(render_card_button(
            "Close tab",
            WardAction::CloseAgentTab(terminal_view_id),
            sub_text,
            appearance,
        )),
        _ => {}
    }
    card.add_child(Container::new(actions.finish()).with_margin_top(3.).finish());

    let background = if snapshot.state.needs_attention() {
        coloru_with_opacity(state_color, 12)
    } else {
        internal_colors::fg_overlay_2(theme).into_solid()
    };
    let card = Container::new(card.finish())
        .with_uniform_padding(8.)
        .with_background(background)
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
        .finish();
    EventHandler::new(card)
        .on_left_mouse_down(move |ctx, _, _| {
            ctx.dispatch_typed_action(WorkspaceAction::Ward(WardAction::OpenAgent(
                terminal_view_id,
            )));
            DispatchEventResult::StopPropagation
        })
        .finish()
}

fn render_card_button(
    label: &str,
    action: WardAction,
    color: ColorU,
    appearance: &Appearance,
) -> Box<dyn Element> {
    let button = Container::new(
        Text::new_inline(label.to_owned(), appearance.ui_font_family(), 11.)
            .with_color(color)
            .finish(),
    )
    .with_horizontal_padding(7.)
    .with_vertical_padding(2.)
    .with_background(coloru_with_opacity(color, 14))
    .with_corner_radius(CornerRadius::with_all(Radius::Pixels(5.)))
    .finish();
    EventHandler::new(button)
        .on_left_mouse_down(move |ctx, _, _| {
            ctx.dispatch_typed_action(WorkspaceAction::Ward(action.clone()));
            DispatchEventResult::StopPropagation
        })
        .finish()
}
