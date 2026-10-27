//! Local Automations list body.
//!
//! Hosted by the Settings → Automations page. Lists automations loaded from
//! the user's `automations/` directory. Each row is styled like an MCP server
//! card: bordered card chrome, name with compact chips (runner type, plus
//! Missed/Disabled/Invalid schedule states), a one-line subtitle (humanized
//! schedule · next · last ran), a chrome-free run (play) icon control, and a
//! "···" overflow menu with Edit config and Move to cloud. Error rows for files
//! that failed to parse keep just the edit control. The "New" button, each row
//! in the collapsible "Suggested" section (always-visible add icon; the whole
//! row is clickable), and "Move to cloud" all open the shared
//! [`LocalAutomationsAgentModal`](super::agent_modal::LocalAutomationsAgentModal),
//! which offers a Warp agent conversation or copying an equivalent prompt
//! for another agent (Claude Code, Codex, ...). The Suggested section's
//! collapse state persists via `LocalAutomationsSettings`. The modal itself
//! is rendered by the hosting settings view (see
//! [`Self::agent_modal_content`]) so its backdrop covers the whole settings
//! surface.

use pathfinder_geometry::vector::vec2f;
use settings::Setting as _;
use warp_core::paths::home_relative_path;
use warp_core::ui::icons::ICON_DIMENSIONS;
use warp_errors::report_if_error;
use warpui::elements::{
    Align, Border, ChildAnchor, ChildView, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, Element, Expanded, Flex, Hoverable, MainAxisSize, MouseStateHandle,
    OffsetPositioning, Padding, ParentAnchor, ParentElement, ParentOffsetBounds,
    PositionedElementAnchor, PositionedElementOffsetBounds, Radius, SavePosition, Stack, Text,
    Wrap,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::ui_components::components::UiComponent;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use crate::appearance::Appearance;
use crate::local_automations::agent_modal::{
    LocalAutomationsAgentModal, LocalAutomationsAgentModalMode,
};
use crate::local_automations::schedule::humanize_schedule;
use crate::local_automations::{
    LocalAutomation, LocalAutomationError, LocalAutomationsScheduler,
    LocalAutomationsSchedulerEvent, SuggestedAutomation,
};
use crate::menu::{Event as MenuEvent, Menu, MenuItemFields};
use crate::settings::LocalAutomationsSettings;
use crate::ui_components::blended_colors;
use crate::ui_components::icons::Icon;
use crate::user_config::{WarpConfig, WarpConfigUpdateEvent};
use crate::view_components::action_button::{ActionButton, PrimaryTheme};
use crate::view_components::DismissibleToast;
use crate::workspace::WorkspaceAction;
use crate::ToastStack;

/// Matches MCP server card list spacing (`SERVER_CARD_LIST_SPACING`).
const ROW_SPACING: f32 = 8.;
/// Matches MCP server card interior spacing (`SERVER_CARD_INTERIOR_SPACING`).
const CARD_INTERIOR_SPACING: f32 = 4.;
/// Matches MCP server card corner radius.
const CARD_CORNER_RADIUS: f32 = 4.;
/// Matches MCP title chip font size.
const TITLE_CHIP_FONT_SIZE: f32 = 10.;
const DESCRIPTION_FONT_SIZE: f32 = 13.;
const ROW_MENU_WIDTH: f32 = 240.;

/// Square size of the bare edit/run icon controls on each row.
const ICON_CONTROL_SIZE: f32 = 16.;

#[derive(Debug, Clone, PartialEq)]
pub enum LocalAutomationsViewAction {
    Run(usize),
    OpenErrorFile(usize),
    /// Opens the agent modal for creating an automation from scratch (the
    /// "New" button).
    OpenNewAutomationModal,
    /// Collapses/expands the "Suggested" section, persisting the choice.
    ToggleSuggestions,
    /// Opens the agent modal for the suggestion at this index in
    /// `SuggestedAutomation::ALL`.
    OpenSuggestionModal(usize),
    /// Toggles the "···" overflow menu for the automation row at this index
    /// in `rows`.
    ToggleRowMenu(usize),
    /// Opens the automation config for the row at this index (the overflow
    /// menu's "Edit config").
    OpenRowConfig(usize),
    /// Opens the move-to-cloud agent modal for the row at this index (the
    /// overflow menu's "Move to cloud").
    OpenMoveToCloudModal(usize),
    /// Toggles `enabled` on the automation at this index (Pause when enabled,
    /// Resume when disabled) by writing the TOML on disk.
    ToggleRowEnabled(usize),
    /// Deletes the automation file for the row at this index.
    DeleteRow(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalAutomationsViewEvent {
    /// The agent modal opened or closed. Hosts re-render so the
    /// settings-level overlay (see [`LocalAutomationsView::agent_modal_content`])
    /// is shown or hidden.
    AgentModalToggled,
}

struct AutomationRow {
    automation: LocalAutomation,
    run_mouse_state: MouseStateHandle,
    menu_mouse_state: MouseStateHandle,
}

struct ErrorRow {
    error: LocalAutomationError,
    edit_mouse_state: MouseStateHandle,
}
fn sort_automations_enabled_first(automations: &mut [LocalAutomation]) {
    // `sort_by_key` is stable, retaining the configuration order within each
    // enabled state.
    automations.sort_by_key(|automation| !automation.enabled);
}

/// Local Automations list body used by the Settings page.
pub struct LocalAutomationsView {
    rows: Vec<AutomationRow>,
    error_rows: Vec<ErrorRow>,
    new_button: ViewHandle<ActionButton>,
    /// Shared agent modal ("Use Warp Agent" / "Copy agent prompt") opened by
    /// the "New" button, Suggested rows, and "Move to cloud". Rendered by
    /// the hosting settings view via [`Self::agent_modal_content`].
    agent_modal: ViewHandle<LocalAutomationsAgentModal>,
    /// One hover state per entry in `SuggestedAutomation::ALL`.
    suggestion_row_mouse_states: Vec<MouseStateHandle>,
    /// Overflow menu shared by all rows' "···" controls; anchored to
    /// whichever control opened it.
    row_menu: ViewHandle<Menu<LocalAutomationsViewAction>>,
    /// Index of the automation row whose overflow menu is open.
    show_row_menu: Option<usize>,
    suggestions_header_mouse_state: MouseStateHandle,
}

impl LocalAutomationsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        // Keep the list in sync while the view is open (files can change on
        // disk via the skill or manual edits).
        ctx.subscribe_to_model(&WarpConfig::handle(ctx), |me, _, event, ctx| {
            if matches!(event, WarpConfigUpdateEvent::LocalAutomations) {
                me.refresh(ctx);
            }
        });
        ctx.subscribe_to_model(
            &LocalAutomationsScheduler::handle(ctx),
            |me, _, event, ctx| {
                if matches!(event, LocalAutomationsSchedulerEvent::StatusUpdated) {
                    me.refresh(ctx);
                }
            },
        );

        let new_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("New", PrimaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(LocalAutomationsViewAction::OpenNewAutomationModal);
            })
        });

        let agent_modal = ctx.add_typed_action_view(LocalAutomationsAgentModal::new);
        // The modal dispatches the selected `WorkspaceAction` itself; we only
        // need to refocus and tell the host to drop the overlay when it
        // closes.
        ctx.subscribe_to_view(&agent_modal, |_, _, _event, ctx| {
            ctx.focus_self();
            ctx.emit(LocalAutomationsViewEvent::AgentModalToggled);
            ctx.notify();
        });

        let suggestion_row_mouse_states = (0..SuggestedAutomation::ALL.len())
            .map(|_| MouseStateHandle::default())
            .collect();

        let row_menu = ctx.add_typed_action_view(|_| {
            let mut menu = Menu::new().with_drop_shadow();
            menu.set_width(ROW_MENU_WIDTH);
            menu
        });
        ctx.subscribe_to_view(&row_menu, |me, _, event, ctx| {
            if let MenuEvent::Close { .. } = event {
                me.show_row_menu = None;
                ctx.focus_self();
                ctx.notify();
            }
        });

        let mut view = Self {
            rows: Vec::new(),
            error_rows: Vec::new(),
            new_button,
            agent_modal,
            suggestion_row_mouse_states,
            row_menu,
            show_row_menu: None,
            suggestions_header_mouse_state: MouseStateHandle::default(),
        };
        view.refresh(ctx);
        view
    }

    /// Save-position anchor for a row's "···" control so the overflow menu
    /// can attach below it.
    fn row_menu_position_id(index: usize) -> String {
        format!("local_automations:row_menu_{index}")
    }

    /// Rebuilds rows from `WarpConfig` and focuses the view. Called when the
    /// settings page is selected.
    pub fn on_open(&mut self, ctx: &mut ViewContext<Self>) {
        self.show_row_menu = None;
        self.agent_modal.update(ctx, |modal, ctx| modal.hide(ctx));
        self.refresh(ctx);
        ctx.focus_self();
    }

    /// The agent modal overlay when it is open, for the hosting settings
    /// view to render above the whole settings surface.
    pub fn agent_modal_content(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        self.agent_modal
            .as_ref(app)
            .is_visible()
            .then(|| ChildView::new(&self.agent_modal).finish())
    }

    /// Opens the shared agent modal for the given flow and notifies the host
    /// so the settings-level overlay renders.
    fn open_agent_modal(
        &mut self,
        mode: LocalAutomationsAgentModalMode,
        ctx: &mut ViewContext<Self>,
    ) {
        self.show_row_menu = None;
        self.agent_modal.update(ctx, |modal, ctx| {
            modal.show(mode, ctx);
        });
        ctx.focus(&self.agent_modal);
        ctx.emit(LocalAutomationsViewEvent::AgentModalToggled);
        ctx.notify();
    }

    fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let (mut automations, errors) = {
            let config = WarpConfig::as_ref(ctx);
            (
                config.local_automations().clone(),
                config.local_automation_errors().clone(),
            )
        };
        sort_automations_enabled_first(&mut automations);

        self.rows = automations
            .into_iter()
            .map(|automation| AutomationRow {
                automation,
                run_mouse_state: MouseStateHandle::default(),
                menu_mouse_state: MouseStateHandle::default(),
            })
            .collect();

        self.error_rows = errors
            .into_iter()
            .map(|error| ErrorRow {
                error,
                edit_mouse_state: MouseStateHandle::default(),
            })
            .collect();

        ctx.notify();
    }

    /// Renders a small label chip next to a card title, matching MCP server
    /// card title chips (`ServerCardView::render_title_chip`).
    fn render_title_chip(label: &str, appearance: &Appearance) -> Box<dyn Element> {
        let chip_color = appearance
            .theme()
            .sub_text_color(appearance.theme().surface_3())
            .into_solid();

        Container::new(
            Text::new(
                label.to_string(),
                appearance.ui_font_family(),
                TITLE_CHIP_FONT_SIZE,
            )
            .with_color(chip_color)
            .finish(),
        )
        .with_background(appearance.theme().surface_3())
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(3.)))
        .with_horizontal_padding(3.)
        .with_vertical_padding(1.)
        .finish()
    }

    /// Shared MCP-style card chrome: border, corner radius, padding, optional
    /// filled background. When `hover_fill` is set and the card is hovered,
    /// fills `surface_3` like installable MCP gallery cards.
    fn render_card_chrome(
        content: Box<dyn Element>,
        filled_background: bool,
        hover_fill: bool,
        is_hovered: bool,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let mut card = Container::new(content)
            .with_padding(Padding::uniform(12.))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(CARD_CORNER_RADIUS)))
            .with_border(Border::all(1.).with_border_fill(theme.outline()));

        if hover_fill && is_hovered {
            card = card.with_background(theme.surface_3());
        } else if filled_background {
            card = card.with_background(theme.surface_1());
        }

        card.finish()
    }

    /// Renders a chrome-free icon control: a bare glyph (no border, fill, or
    /// square hover background) that brightens on hover, shows `tooltip`, and
    /// dispatches `action` on click.
    fn render_icon_control(
        icon: Icon,
        mouse_state: &MouseStateHandle,
        tooltip: &'static str,
        action: LocalAutomationsViewAction,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let idle_color = theme.nonactive_ui_text_color();
        let hover_color = theme.active_ui_text_color();
        Hoverable::new(mouse_state.clone(), |mouse_state| {
            let color = if mouse_state.is_hovered() {
                hover_color
            } else {
                idle_color
            };
            let icon_element = ConstrainedBox::new(icon.to_warpui_icon(color).finish())
                .with_width(ICON_CONTROL_SIZE)
                .with_height(ICON_CONTROL_SIZE)
                .finish();
            if !mouse_state.is_hovered() {
                return icon_element;
            }
            // Tooltip overlay above the icon, mirroring ActionButton's
            // tooltip positioning.
            let tooltip_element = appearance
                .ui_builder()
                .tool_tip(tooltip.to_string())
                .build()
                .finish();
            let mut stack = Stack::new().with_child(icon_element);
            stack.add_positioned_overlay_child(
                tooltip_element,
                OffsetPositioning::offset_from_parent(
                    vec2f(0., -4.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::TopMiddle,
                    ChildAnchor::BottomMiddle,
                ),
            );
            stack.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(action.clone());
        })
        .finish()
    }

    fn render_header(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();

        let description = Text::new(
            "Automations run recurring work for you: an agent or command that fires on a \
             schedule or in response to events. Set one up with a Warp agent, on this machine \
             or in the cloud."
                .to_string(),
            appearance.ui_font_family(),
            DESCRIPTION_FONT_SIZE,
        )
        .with_color(theme.nonactive_ui_text_color().into())
        .soft_wrap(true)
        .finish();

        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Expanded::new(1., Align::new(description).left().finish()).finish())
            .with_child(ChildView::new(&self.new_button).finish())
            .finish()
    }

    fn render_automation_row(
        &self,
        row: &AutomationRow,
        index: usize,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let automation = &row.automation;
        let status = LocalAutomationsScheduler::handle(app)
            .as_ref(app)
            .status_for(automation);

        // Title + chips, matching MCP server cards.
        let name_color = if automation.enabled {
            blended_colors::text_main(theme, theme.surface_1())
        } else {
            blended_colors::text_disabled(theme, theme.surface_1())
        };
        let mut title_wrap = Wrap::row()
            .with_spacing(CARD_INTERIOR_SPACING)
            .with_run_spacing(CARD_INTERIOR_SPACING)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Text::new(
                    automation.name.clone(),
                    appearance.ui_font_family(),
                    appearance.ui_builder().ui_font_size(),
                )
                .with_color(name_color)
                .finish(),
            )
            .with_child(Self::render_title_chip(
                automation.runner.display_label(),
                appearance,
            ));
        if status.missed {
            title_wrap = title_wrap.with_child(Self::render_title_chip("Missed", appearance));
        }
        if status.invalid_schedule {
            title_wrap =
                title_wrap.with_child(Self::render_title_chip("Invalid schedule", appearance));
        }
        if !automation.enabled {
            title_wrap = title_wrap.with_child(Self::render_title_chip("Disabled", appearance));
        }

        // One short subtitle line: humanized schedule plus next/last times.
        let mut subtitle_parts = vec![humanize_schedule(&automation.schedule)];
        if let Some(next) = status.next_fragment() {
            subtitle_parts.push(next);
        }
        if let Some(last) = status.last_ran_fragment() {
            subtitle_parts.push(last);
        }
        let subtitle = subtitle_parts.join(" · ");

        let info_column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(CARD_INTERIOR_SPACING)
            .with_child(title_wrap.finish())
            .with_child(
                Text::new(
                    subtitle,
                    appearance.ui_font_family(),
                    appearance.ui_builder().ui_font_size(),
                )
                .with_color(blended_colors::text_disabled(theme, theme.surface_1()))
                .finish(),
            )
            .finish();

        let actions = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(CARD_INTERIOR_SPACING)
            .with_child(Self::render_icon_control(
                Icon::Play,
                &row.run_mouse_state,
                "Run now",
                LocalAutomationsViewAction::Run(index),
                appearance,
            ))
            .with_child(
                SavePosition::new(
                    Self::render_icon_control(
                        Icon::DotsHorizontal,
                        &row.menu_mouse_state,
                        "More actions",
                        LocalAutomationsViewAction::ToggleRowMenu(index),
                        appearance,
                    ),
                    &Self::row_menu_position_id(index),
                )
                .finish(),
            )
            .finish();

        let content = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Expanded::new(1., info_column).finish())
            .with_child(actions)
            .finish();

        Container::new(Self::render_card_chrome(
            content,
            true,  /* filled_background */
            false, /* hover_fill */
            false, /* is_hovered */
            appearance,
        ))
        .with_margin_bottom(ROW_SPACING)
        .finish()
    }

    fn render_error_row(
        &self,
        row: &ErrorRow,
        index: usize,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let info_column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_spacing(CARD_INTERIOR_SPACING)
            .with_child(
                Text::new(
                    format!("{} failed to load", row.error.file_name),
                    appearance.ui_font_family(),
                    appearance.ui_builder().ui_font_size(),
                )
                .with_color(theme.ui_error_color())
                .finish(),
            )
            .with_child(
                Text::new(
                    row.error.error_message.clone(),
                    appearance.ui_font_family(),
                    appearance.ui_builder().ui_font_size(),
                )
                .with_color(blended_colors::text_disabled(theme, theme.surface_1()))
                .soft_wrap(true)
                .finish(),
            )
            .finish();

        let content = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Expanded::new(1., info_column).finish())
            .with_child(Self::render_icon_control(
                Icon::Pencil,
                &row.edit_mouse_state,
                "Edit config",
                LocalAutomationsViewAction::OpenErrorFile(index),
                appearance,
            ))
            .finish();

        Container::new(Self::render_card_chrome(
            content, true,  /* filled_background */
            false, /* hover_fill */
            false, /* is_hovered */
            appearance,
        ))
        .with_margin_bottom(ROW_SPACING)
        .finish()
    }

    fn render_empty_state(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        Text::new(
            format!(
                "Nothing here yet. An automation is a job that runs on this machine; an agent \
                 prompt or a shell command, in a directory you choose. Hit New and an agent will \
                 set one up with you, pick a suggestion below, or drop a TOML file in {}.",
                home_relative_path(&crate::user_config::automations_dir())
            ),
            appearance.ui_font_family(),
            DESCRIPTION_FONT_SIZE,
        )
        .with_color(theme.nonactive_ui_text_color().into())
        .soft_wrap(true)
        .finish()
    }

    /// Renders the collapsible "Suggested" section: a chevron header plus one
    /// row per recipe when expanded. Always shown, even with an empty list.
    fn render_suggestions_section(
        &self,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let collapsed = *LocalAutomationsSettings::as_ref(app).suggestions_collapsed;

        let ui_font_family = appearance.ui_font_family();
        let ui_font_size = appearance.ui_font_size();
        let header_color = theme.active_ui_text_color();
        let chevron_color = theme.nonactive_ui_text_color();
        let header = Hoverable::new(
            self.suggestions_header_mouse_state.clone(),
            move |_mouse_state| {
                let chevron = if collapsed {
                    Icon::ChevronRight
                } else {
                    Icon::ChevronDown
                };
                let chevron_element = Container::new(
                    ConstrainedBox::new(chevron.to_warpui_icon(chevron_color).finish())
                        .with_width(16.)
                        .with_height(16.)
                        .finish(),
                )
                .with_margin_right(6.)
                .finish();

                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(chevron_element)
                    .with_child(
                        Text::new_inline("Suggested", ui_font_family, ui_font_size)
                            .with_color(header_color.into())
                            .with_style(Properties::default().weight(Weight::Bold))
                            .finish(),
                    )
                    .finish()
            },
        )
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(LocalAutomationsViewAction::ToggleSuggestions);
        })
        .finish();

        let mut section = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(header)
                    .with_margin_top(16.)
                    .with_margin_bottom(ROW_SPACING)
                    .finish(),
            );

        if !collapsed {
            for (index, suggestion) in SuggestedAutomation::ALL.iter().enumerate() {
                section.add_child(self.render_suggestion_row(*suggestion, index, appearance));
            }
        }

        section.finish()
    }

    /// Renders one "Suggested" recipe as an MCP-style installable card: bordered
    /// chrome, always-visible add (+) icon, hover fill, and a full-card click
    /// that opens the agent modal for the recipe.
    fn render_suggestion_row(
        &self,
        suggestion: SuggestedAutomation,
        index: usize,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let mouse_state = self
            .suggestion_row_mouse_states
            .get(index)
            .cloned()
            .unwrap_or_default();

        let row = Hoverable::new(mouse_state, |state| {
            let info_column = Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_spacing(CARD_INTERIOR_SPACING)
                .with_child(
                    Text::new(
                        suggestion.title().to_string(),
                        appearance.ui_font_family(),
                        appearance.ui_builder().ui_font_size(),
                    )
                    .with_color(blended_colors::text_main(theme, theme.surface_1()))
                    .finish(),
                )
                .with_child(
                    Text::new(
                        suggestion.description().to_string(),
                        appearance.ui_font_family(),
                        appearance.ui_builder().ui_font_size(),
                    )
                    .with_color(blended_colors::text_disabled(theme, theme.surface_1()))
                    .finish(),
                )
                .finish();

            let add_icon = ConstrainedBox::new(
                warpui::elements::Icon::new(
                    Icon::Plus.into(),
                    blended_colors::text_main(theme, theme.background()),
                )
                .finish(),
            )
            .with_width(ICON_DIMENSIONS)
            .with_height(ICON_DIMENSIONS)
            .finish();

            let content = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Expanded::new(1., info_column).finish())
                .with_child(add_icon)
                .finish();

            Self::render_card_chrome(
                content,
                false, /* filled_background */
                true,  /* hover_fill */
                state.is_hovered() || state.is_clicked(),
                appearance,
            )
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(LocalAutomationsViewAction::OpenSuggestionModal(index));
        })
        .finish();

        Container::new(row).with_margin_bottom(ROW_SPACING).finish()
    }

    /// Writes `enabled` for the automation at `index` (Pause when currently
    /// enabled, Resume when disabled). Relies on the filesystem watcher to
    /// reload the list after the TOML changes.
    fn toggle_row_enabled(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let Some(path) = row.automation.source_path.clone() else {
            return;
        };
        let enabled = !row.automation.enabled;
        #[cfg(feature = "local_fs")]
        {
            if let Err(e) = WarpConfig::set_local_automation_enabled(&path, enabled) {
                let window_id = ctx.window_id();
                ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                    toast_stack.add_ephemeral_toast(
                        DismissibleToast::error(format!(
                            "Couldn't {} automation: {e}",
                            if enabled { "resume" } else { "pause" }
                        )),
                        window_id,
                        ctx,
                    );
                });
            }
        }
        #[cfg(not(feature = "local_fs"))]
        {
            let _ = (path, enabled);
        }
    }

    /// Deletes the automation file for the row at `index`. Relies on the
    /// filesystem watcher to reload the list after the file is removed.
    fn delete_row(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let Some(path) = row.automation.source_path.clone() else {
            return;
        };
        #[cfg(feature = "local_fs")]
        {
            if let Err(e) = WarpConfig::delete_local_automation(&path) {
                let window_id = ctx.window_id();
                ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                    toast_stack.add_ephemeral_toast(
                        DismissibleToast::error(format!("Couldn't delete automation: {e}")),
                        window_id,
                        ctx,
                    );
                });
            }
        }
        #[cfg(not(feature = "local_fs"))]
        {
            let _ = path;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn automation(name: &str, enabled: bool) -> LocalAutomation {
        LocalAutomation::parse(&format!(
            r#"
name = "{name}"
enabled = {enabled}
schedule = "@daily"
cwd = "/tmp"

[runner]
type = "shell"
command = "true"
"#
        ))
        .unwrap()
    }

    #[test]
    fn sort_automations_enabled_first_is_stable_within_each_state() {
        let mut automations = vec![
            automation("disabled first", false),
            automation("enabled first", true),
            automation("disabled second", false),
            automation("enabled second", true),
        ];

        sort_automations_enabled_first(&mut automations);

        assert_eq!(
            automations
                .iter()
                .map(|automation| automation.name.as_str())
                .collect::<Vec<_>>(),
            [
                "enabled first",
                "enabled second",
                "disabled first",
                "disabled second",
            ],
        );
    }
}

impl Entity for LocalAutomationsView {
    type Event = LocalAutomationsViewEvent;
}

impl View for LocalAutomationsView {
    fn ui_name() -> &'static str {
        "LocalAutomationsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);

        let mut content = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(
                Container::new(self.render_header(appearance))
                    .with_margin_bottom(16.)
                    .finish(),
            );

        if self.rows.is_empty() && self.error_rows.is_empty() {
            content.add_child(
                Container::new(self.render_empty_state(appearance))
                    .with_margin_bottom(ROW_SPACING)
                    .finish(),
            );
        } else {
            for (index, row) in self.rows.iter().enumerate() {
                content.add_child(self.render_automation_row(row, index, appearance, app));
            }
            for (index, row) in self.error_rows.iter().enumerate() {
                content.add_child(self.render_error_row(row, index, appearance));
            }
        }

        content.add_child(self.render_suggestions_section(appearance, app));

        let page = content.finish();

        let Some(index) = self.show_row_menu else {
            return page;
        };

        let mut stack = Stack::new();
        stack.add_child(page);
        stack.add_positioned_child(
            ChildView::new(&self.row_menu).finish(),
            OffsetPositioning::offset_from_save_position_element(
                Self::row_menu_position_id(index),
                vec2f(0., 4.),
                PositionedElementOffsetBounds::WindowByPosition,
                PositionedElementAnchor::BottomRight,
                ChildAnchor::TopRight,
            ),
        );
        stack.finish()
    }
}

impl TypedActionView for LocalAutomationsView {
    type Action = LocalAutomationsViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            LocalAutomationsViewAction::Run(index) => {
                if let Some(row) = self.rows.get(*index) {
                    ctx.dispatch_typed_action(&WorkspaceAction::RunLocalAutomation {
                        automation: row.automation.clone(),
                    });
                }
            }
            LocalAutomationsViewAction::OpenErrorFile(index) => {
                if let Some(row) = self.error_rows.get(*index) {
                    ctx.dispatch_typed_action(&WorkspaceAction::OpenLocalAutomationConfig {
                        path: row.error.file_path.clone(),
                    });
                }
            }
            LocalAutomationsViewAction::ToggleSuggestions => {
                let collapsed = *LocalAutomationsSettings::as_ref(ctx).suggestions_collapsed;
                LocalAutomationsSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.suggestions_collapsed.set_value(!collapsed, ctx));
                });
                ctx.notify();
            }
            LocalAutomationsViewAction::OpenNewAutomationModal => {
                self.open_agent_modal(LocalAutomationsAgentModalMode::NewAutomation, ctx);
            }
            LocalAutomationsViewAction::OpenSuggestionModal(index) => {
                if let Some(suggestion) = SuggestedAutomation::ALL.get(*index) {
                    self.open_agent_modal(
                        LocalAutomationsAgentModalMode::SetUpSuggestion(*suggestion),
                        ctx,
                    );
                }
            }
            LocalAutomationsViewAction::ToggleRowMenu(index) => {
                if self.show_row_menu == Some(*index) {
                    self.show_row_menu = None;
                    ctx.focus_self();
                } else if let Some(row) = self.rows.get(*index) {
                    self.show_row_menu = Some(*index);
                    let has_source_path = row.automation.source_path.is_some();
                    let mut items = Vec::new();
                    if has_source_path {
                        items.push(
                            MenuItemFields::new("Edit config")
                                .with_icon(Icon::Pencil)
                                .with_on_select_action(LocalAutomationsViewAction::OpenRowConfig(
                                    *index,
                                ))
                                .into_item(),
                        );
                        let (pause_label, pause_icon) = if row.automation.enabled {
                            ("Pause", Icon::Pause)
                        } else {
                            ("Resume", Icon::Play)
                        };
                        items.push(
                            MenuItemFields::new(pause_label)
                                .with_icon(pause_icon)
                                .with_on_select_action(
                                    LocalAutomationsViewAction::ToggleRowEnabled(*index),
                                )
                                .into_item(),
                        );
                    }
                    items.push(
                        MenuItemFields::new("Move to cloud")
                            .with_icon(Icon::Cloud)
                            .with_on_select_action(
                                LocalAutomationsViewAction::OpenMoveToCloudModal(*index),
                            )
                            .into_item(),
                    );
                    if has_source_path {
                        items.push(
                            MenuItemFields::new("Delete")
                                .with_icon(Icon::Trash)
                                .with_override_text_color(
                                    Appearance::as_ref(ctx).theme().ansi_fg_red(),
                                )
                                .with_on_select_action(LocalAutomationsViewAction::DeleteRow(
                                    *index,
                                ))
                                .into_item(),
                        );
                    }
                    self.row_menu.update(ctx, |menu, ctx| {
                        menu.set_items(items, ctx);
                    });
                    ctx.focus(&self.row_menu);
                }
                ctx.notify();
            }
            LocalAutomationsViewAction::OpenRowConfig(index) => {
                if let Some(path) = self
                    .rows
                    .get(*index)
                    .and_then(|row| row.automation.source_path.clone())
                {
                    ctx.dispatch_typed_action(&WorkspaceAction::OpenLocalAutomationConfig { path });
                }
            }
            LocalAutomationsViewAction::OpenMoveToCloudModal(index) => {
                if let Some(row) = self.rows.get(*index) {
                    self.open_agent_modal(
                        LocalAutomationsAgentModalMode::MoveToCloud(Box::new(
                            row.automation.clone(),
                        )),
                        ctx,
                    );
                }
            }
            LocalAutomationsViewAction::ToggleRowEnabled(index) => {
                self.toggle_row_enabled(*index, ctx);
            }
            LocalAutomationsViewAction::DeleteRow(index) => {
                self.delete_row(*index, ctx);
            }
        }
    }
}
