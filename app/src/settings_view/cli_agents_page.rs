//! The "Third party CLI agents" settings page, shown under the Agents umbrella.
//!
//! Everything on this page controls third-party coding agents (Claude Code,
//! Codex, Gemini CLI) rather than Warp's own AI, so its settings are always
//! interactive regardless of the global AI toggle.

use std::cell::RefCell;
use std::collections::HashMap;

use enum_iterator::all;
use markdown_parser::{FormattedText, FormattedTextFragment, FormattedTextLine};
use regex::Regex;
use settings::{Setting, ToggleableSetting};
use warp_core::features::FeatureFlag;
use warp_errors::report_if_error;
use warpui::elements::{
    ChildView, Container, CornerRadius, CrossAxisAlignment, Element, Empty, Flex,
    FormattedTextElement, HighlightedHyperlink, MainAxisAlignment, MainAxisSize, MouseStateHandle,
    ParentElement, Radius, Shrinkable,
};
use warpui::keymap::ContextPredicate;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{
    Action, AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle, id,
};

use super::ai_shared::{
    render_ai_feature_switch, render_ai_setting_toggle, render_toolbar_layout_editor, styles,
    update_editor_interaction_state,
};
use super::settings_page::{
    AdditionalInfo, CONTENT_FONT_SIZE, LocalOnlyIconState, MatchData, PageTitle, PageType,
    SettingsPageMeta, SettingsPageViewHandle, SettingsWidget, ToggleState, build_toggle_element,
    render_body_item_label,
};
use super::{SettingsAction, SettingsSection, ToggleSettingActionPair, flags};
use crate::ai::blocklist::agent_view::agent_input_footer::editor::{
    AgentToolbarEditorMode, AgentToolbarInlineEditor,
};
use crate::appearance::Appearance;
use crate::menu::{MenuItem, MenuItemFields};
use crate::settings::{
    AISettings, AISettingsChangedEvent, AutoDismissRichInputAfterSubmit,
    AutoOpenRichInputOnCLIAgentStart, AutoToggleRichInput, ShouldRenderCLIAgentToolbar,
    SubmitRichInputOnCtrlEnter,
};
use crate::terminal::CLIAgent;
use crate::util::bindings;
use crate::view_components::dropdown::DropdownAction;
use crate::view_components::{Dropdown, SubmittableTextInput, SubmittableTextInputEvent};
use crate::ward::chimney as ward_chimney;
use crate::ward::claude_launch as ward_claude_launch;
use crate::ward::claude_usage as ward_claude_usage;
use crate::ward::codex_usage as ward_codex_usage;
use crate::{TelemetryEvent, send_telemetry_from_ctx};

const PAGE_TITLE: &str = "Third party CLI agents";

pub struct CLIAgentsPageView {
    page: PageType<Self>,
    local_only_icon_tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
    cli_agent_footer_command_editor: ViewHandle<SubmittableTextInput>,
    cli_agent_footer_command_mouse_state_handles: Vec<MouseStateHandle>,
    cli_agent_footer_command_agent_dropdowns: Vec<ViewHandle<Dropdown<CLIAgentsPageAction>>>,
    cli_agent_toolbar_inline_editor: ViewHandle<AgentToolbarInlineEditor>,
    /// Ward (OSS build) Chimney settings inputs.
    chimney_base_url_editor: ViewHandle<SubmittableTextInput>,
    chimney_board_editor: ViewHandle<SubmittableTextInput>,
    chimney_token_editor: ViewHandle<SubmittableTextInput>,
    chimney_remove_token_mouse_state: MouseStateHandle,
    chimney_email_editor: ViewHandle<SubmittableTextInput>,
    chimney_password_editor: ViewHandle<SubmittableTextInput>,
    chimney_remove_account_mouse_state: MouseStateHandle,
}

impl CLIAgentsPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let cli_agent_footer_command_editor = ctx.add_typed_action_view(|ctx| {
            let mut input =
                SubmittableTextInput::new(ctx).validate_on_edit(|s| Regex::new(s).is_ok());
            input.set_placeholder_text("command (supports regex)", ctx);
            input
        });
        update_editor_interaction_state(
            cli_agent_footer_command_editor.as_ref(ctx).editor().clone(),
            true,
            ctx,
        );
        ctx.subscribe_to_view(
            &cli_agent_footer_command_editor,
            |_, _, event, ctx| match event {
                SubmittableTextInputEvent::Submit(command) => {
                    AISettings::handle(ctx).update(ctx, |settings, ctx| {
                        settings.add_cli_agent_footer_enabled_command(command, ctx);
                    });
                }
                SubmittableTextInputEvent::Escape => ctx.emit(CLIAgentsPageEvent::FocusModal),
            },
        );

        let cli_agent_footer_command_mouse_state_handles = AISettings::as_ref(ctx)
            .cli_agent_footer_enabled_commands
            .value()
            .keys()
            .map(|_| Default::default())
            .collect();

        let cli_agent_toolbar_inline_editor = ctx.add_typed_action_view(|ctx| {
            AgentToolbarInlineEditor::new(AgentToolbarEditorMode::CLIAgent, ctx)
        });

        ctx.subscribe_to_model(&AISettings::handle(ctx), |me, _, event, ctx| {
            // Adding or removing a command changes the length of the command
            // list, so both the per-row mouse states and the per-row agent
            // dropdowns have to be rebuilt to stay index-aligned with it.
            if let AISettingsChangedEvent::CLIAgentToolbarEnabledCommands { .. } = event {
                me.cli_agent_footer_command_mouse_state_handles = AISettings::as_ref(ctx)
                    .cli_agent_footer_enabled_commands
                    .value()
                    .keys()
                    .map(|_| Default::default())
                    .collect();
                me.cli_agent_footer_command_agent_dropdowns = Self::create_cli_agent_dropdowns(ctx);
            }
            ctx.notify();
        });

        let (chimney_base_url_editor, chimney_board_editor, chimney_token_editor) =
            Self::create_chimney_editors(ctx);
        let (chimney_email_editor, chimney_password_editor) =
            Self::create_chimney_account_editors(ctx);
        if ctx.has_singleton_model::<ward_chimney::model::ChimneyModel>() {
            // Sign-in status and the board title come from fetches.
            ctx.subscribe_to_model(
                &ward_chimney::model::ChimneyModel::handle(ctx),
                |_, _, _, ctx| ctx.notify(),
            );
        }

        Self {
            page: Self::build_page(),
            local_only_icon_tooltip_states: Default::default(),
            cli_agent_footer_command_editor,
            cli_agent_footer_command_mouse_state_handles,
            cli_agent_footer_command_agent_dropdowns: Self::create_cli_agent_dropdowns(ctx),
            cli_agent_toolbar_inline_editor,
            chimney_base_url_editor,
            chimney_board_editor,
            chimney_token_editor,
            chimney_remove_token_mouse_state: Default::default(),
            chimney_email_editor,
            chimney_password_editor,
            chimney_remove_account_mouse_state: Default::default(),
        }
    }

    /// Base URL, board id and access token inputs of the Chimney section. Each saves on Enter.
    fn create_chimney_editors(
        ctx: &mut ViewContext<Self>,
    ) -> (
        ViewHandle<SubmittableTextInput>,
        ViewHandle<SubmittableTextInput>,
        ViewHandle<SubmittableTextInput>,
    ) {
        let base_url_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx)
                .validate_on_edit(|url| ward_chimney::normalize_base_url(url).is_some());
            input.set_placeholder_text(ward_chimney::DEFAULT_BASE_URL, ctx);
            input
        });
        let board_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx)
                .validate_on_edit(|id| id.trim().parse::<u64>().is_ok_and(|id| id > 0));
            input.set_placeholder_text("board id", ctx);
            input
        });
        let token_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new_with_password(true, ctx);
            input.set_placeholder_text("access token", ctx);
            input
        });
        for editor in [&base_url_editor, &board_editor, &token_editor] {
            update_editor_interaction_state(editor.as_ref(ctx).editor().clone(), true, ctx);
        }
        ctx.subscribe_to_view(&base_url_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(url) => {
                ward_chimney::set_base_url(url, ctx);
                ctx.notify();
            }
            SubmittableTextInputEvent::Escape => ctx.emit(CLIAgentsPageEvent::FocusModal),
        });
        ctx.subscribe_to_view(&board_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(id) => {
                if let Ok(id) = id.trim().parse::<u64>() {
                    ward_chimney::set_board_id(id, ctx);
                }
                ctx.notify();
            }
            SubmittableTextInputEvent::Escape => ctx.emit(CLIAgentsPageEvent::FocusModal),
        });
        ctx.subscribe_to_view(&token_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(token) => {
                ward_chimney::set_access_token(token, ctx);
                Self::chimney_credentials_changed(ctx);
            }
            SubmittableTextInputEvent::Escape => ctx.emit(CLIAgentsPageEvent::FocusModal),
        });
        (base_url_editor, board_editor, token_editor)
    }

    /// Email and password inputs of the Chimney service account. Each saves on Enter into the
    /// secure storage; the password input is masked.
    fn create_chimney_account_editors(
        ctx: &mut ViewContext<Self>,
    ) -> (ViewHandle<SubmittableTextInput>, ViewHandle<SubmittableTextInput>) {
        let email_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new(ctx);
            input.set_placeholder_text("service account email", ctx);
            input
        });
        let password_editor = ctx.add_typed_action_view(|ctx| {
            let mut input = SubmittableTextInput::new_with_password(true, ctx);
            input.set_placeholder_text("service account password", ctx);
            input
        });
        for editor in [&email_editor, &password_editor] {
            update_editor_interaction_state(editor.as_ref(ctx).editor().clone(), true, ctx);
        }
        ctx.subscribe_to_view(&email_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(email) => {
                ward_chimney::set_email(email, ctx);
                Self::chimney_credentials_changed(ctx);
            }
            SubmittableTextInputEvent::Escape => ctx.emit(CLIAgentsPageEvent::FocusModal),
        });
        ctx.subscribe_to_view(&password_editor, |_, _, event, ctx| match event {
            SubmittableTextInputEvent::Submit(password) => {
                ward_chimney::set_password(password, ctx);
                Self::chimney_credentials_changed(ctx);
            }
            SubmittableTextInputEvent::Escape => ctx.emit(CLIAgentsPageEvent::FocusModal),
        });
        (email_editor, password_editor)
    }

    /// Fetches the configured board with the new credentials, so the status updates now.
    fn chimney_credentials_changed(ctx: &mut ViewContext<Self>) {
        ward_chimney::model::refresh_board(ward_chimney::board_id(ctx), ctx);
        ward_chimney::model::ensure_polling(ctx);
        ctx.notify();
    }

    fn build_page() -> PageType<Self> {
        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
            Box::new(CLIAgentWidget::default()),
            Box::new(CLIAgentAutoToggleRichInputWidget::default()),
            Box::new(CLIAgentAutoOpenRichInputWidget::default()),
            Box::new(CLIAgentAutoDismissRichInputWidget::default()),
            Box::new(CLIAgentSubmitRichInputWidget::default()),
            Box::new(CLIAgentCommandsWidget),
            Box::new(CLIAgentToolbarLayoutWidget),
            Box::new(ClaudeUsageChipWidget::default()),
            Box::new(CodexUsageChipWidget::default()),
            Box::new(ClaudeNoFlickerWidget::default()),
            Box::new(ChimneyWidget::default()),
        ];
        PageType::new_uncategorized(widgets, Some(PageTitle::new(PAGE_TITLE)))
    }

    fn create_cli_agent_dropdowns(
        ctx: &mut ViewContext<Self>,
    ) -> Vec<ViewHandle<Dropdown<CLIAgentsPageAction>>> {
        let entries: Vec<(String, CLIAgent)> = AISettings::as_ref(ctx)
            .cli_agent_footer_enabled_commands
            .value()
            .iter()
            .map(|(pattern, agent_value)| {
                (pattern.clone(), CLIAgent::from_serialized_name(agent_value))
            })
            .collect();

        entries
            .into_iter()
            .map(|(pattern_clone, current_agent)| {
                ctx.add_typed_action_view(move |ctx| {
                    let mut dropdown = Dropdown::new(ctx);
                    dropdown.set_top_bar_max_width(160.);
                    dropdown.set_menu_width(180., ctx);
                    dropdown.set_main_axis_size(MainAxisSize::Min, ctx);

                    let mut items: Vec<MenuItem<DropdownAction>> = Vec::new();

                    for agent in all::<CLIAgent>() {
                        if matches!(agent, CLIAgent::Unknown) {
                            continue;
                        }
                        let icon = agent.icon();
                        let mut fields = MenuItemFields::new(agent.display_name())
                            .with_on_select_action(DropdownAction::select_action_and_close(
                                CLIAgentsPageAction::SetCLIAgentForCommand {
                                    pattern: pattern_clone.clone(),
                                    agent: Some(agent),
                                },
                            ));
                        if let Some(icon) = icon {
                            fields = fields.with_icon(icon);
                        }
                        items.push(fields.into_item());
                    }

                    items.push(
                        MenuItemFields::new("Other")
                            .with_on_select_action(DropdownAction::select_action_and_close(
                                CLIAgentsPageAction::SetCLIAgentForCommand {
                                    pattern: pattern_clone.clone(),
                                    agent: None,
                                },
                            ))
                            .into_item(),
                    );

                    dropdown.set_rich_items(items, ctx);

                    dropdown.set_menu_header_text_override(|label| {
                        if label == "Other" {
                            "Select coding agent".to_string()
                        } else {
                            label.to_string()
                        }
                    });

                    let selected_name = if matches!(current_agent, CLIAgent::Unknown) {
                        "Other"
                    } else {
                        current_agent.display_name()
                    };
                    dropdown.set_selected_by_name(selected_name, ctx);

                    dropdown
                })
            })
            .collect()
    }
}

impl Entity for CLIAgentsPageView {
    type Event = CLIAgentsPageEvent;
}

impl View for CLIAgentsPageView {
    fn ui_name() -> &'static str {
        "CLIAgentsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

pub enum CLIAgentsPageEvent {
    FocusModal,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CLIAgentsPageAction {
    ToggleCLIAgentToolbar,
    ToggleAutoToggleRichInput,
    ToggleAutoOpenRichInputOnCLIAgentStart,
    ToggleAutoDismissRichInputAfterSubmit,
    ToggleSubmitRichInputOnCtrlEnter,
    RemoveCLIAgentToolbarEnabledCommand(String),
    SetCLIAgentForCommand {
        pattern: String,
        agent: Option<CLIAgent>,
    },
    /// Ward: removes the Chimney access token from the secure storage.
    RemoveChimneyToken,
    /// Ward: removes the Chimney service account email and password from the secure storage.
    RemoveChimneyAccount,
    /// Ward: shows or hides the Claude subscription usage chip in the header.
    ToggleClaudeUsageChip,
    /// Ward: shows or hides the Codex plan usage chip in the header.
    ToggleCodexUsageChip,
    /// Ward: starts Claude Code tabs with `CLAUDE_CODE_NO_FLICKER=1`.
    ToggleClaudeNoFlicker,
}

impl TypedActionView for CLIAgentsPageView {
    type Action = CLIAgentsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CLIAgentsPageAction::ToggleCLIAgentToolbar => {
                match AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings
                        .should_render_cli_agent_footer
                        .toggle_and_save_value(ctx)
                }) {
                    Ok(new_value) => {
                        send_telemetry_from_ctx!(
                            TelemetryEvent::ToggleCLIAgentToolbarSetting {
                                is_enabled: new_value,
                            },
                            ctx
                        );
                    }
                    Err(e) => {
                        log::warn!("Failed to set value for CLI Agent Footer setting: {e:?}");
                    }
                }
                ctx.notify();
            }
            CLIAgentsPageAction::ToggleAutoToggleRichInput => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.auto_toggle_rich_input.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            CLIAgentsPageAction::ToggleAutoOpenRichInputOnCLIAgentStart => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(
                        settings
                            .auto_open_rich_input_on_cli_agent_start
                            .toggle_and_save_value(ctx)
                    );
                });
                ctx.notify();
            }
            CLIAgentsPageAction::ToggleAutoDismissRichInputAfterSubmit => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(
                        settings
                            .auto_dismiss_rich_input_after_submit
                            .toggle_and_save_value(ctx)
                    );
                });
                ctx.notify();
            }
            CLIAgentsPageAction::ToggleSubmitRichInputOnCtrlEnter => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.submit_on_ctrl_enter.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            CLIAgentsPageAction::RemoveCLIAgentToolbarEnabledCommand(command) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings.remove_cli_agent_footer_enabled_command(command, ctx);
                });
            }
            CLIAgentsPageAction::SetCLIAgentForCommand { pattern, agent } => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    settings.set_cli_agent_for_command(pattern, *agent, ctx);
                });
            }
            CLIAgentsPageAction::RemoveChimneyToken => {
                ward_chimney::set_access_token("", ctx);
                Self::chimney_credentials_changed(ctx);
            }
            CLIAgentsPageAction::RemoveChimneyAccount => {
                ward_chimney::set_email("", ctx);
                ward_chimney::set_password("", ctx);
                Self::chimney_credentials_changed(ctx);
            }
            CLIAgentsPageAction::ToggleClaudeUsageChip => {
                let enabled = ward_claude_usage::is_enabled_by_user(ctx);
                ward_claude_usage::set_enabled_by_user(!enabled, ctx);
                // Turning the chip on fetches now instead of at the next poll; turning it off
                // re-renders the headers without it.
                ward_claude_usage::model::refresh(ctx);
                ctx.notify();
            }
            CLIAgentsPageAction::ToggleCodexUsageChip => {
                let enabled = ward_codex_usage::is_enabled_by_user(ctx);
                ward_codex_usage::set_enabled_by_user(!enabled, ctx);
                ward_codex_usage::model::refresh(ctx);
                ctx.notify();
            }
            CLIAgentsPageAction::ToggleClaudeNoFlicker => {
                let enabled = ward_claude_launch::is_no_flicker_enabled(ctx);
                ward_claude_launch::set_no_flicker_enabled(!enabled, ctx);
                ctx.notify();
            }
        }
    }
}

impl SettingsPageMeta for CLIAgentsPageView {
    fn section() -> SettingsSection {
        SettingsSection::ThirdPartyCLIAgents
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::AgentMode.is_enabled()
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

impl From<ViewHandle<CLIAgentsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<CLIAgentsPageView>) -> Self {
        SettingsPageViewHandle::CLIAgents(view_handle)
    }
}

pub fn init_actions_from_parent_view<T: Action + Clone>(
    app: &mut AppContext,
    context: &ContextPredicate,
    builder: fn(SettingsAction) -> T,
) {
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![
            ToggleSettingActionPair::new(
                "coding agent toolbar",
                builder(SettingsAction::CLIAgents(
                    CLIAgentsPageAction::ToggleCLIAgentToolbar,
                )),
                context,
                flags::CLI_AGENT_FOOTER_ENABLED,
            )
            .with_group(bindings::BindingGroup::WarpAi),
        ],
        app,
    );
    ToggleSettingActionPair::add_toggle_setting_action_pairs_as_bindings(
        vec![
            ToggleSettingActionPair::new(
                "auto show or hide Rich Input based on agent status",
                builder(SettingsAction::CLIAgents(
                    CLIAgentsPageAction::ToggleAutoToggleRichInput,
                )),
                &(context.clone() & id!(flags::CLI_AGENT_FOOTER_ENABLED)),
                flags::AUTO_TOGGLE_RICH_INPUT_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| FeatureFlag::CLIAgentRichInput.is_enabled()),
            ToggleSettingActionPair::new(
                "auto open Rich Input when a coding agent session starts",
                builder(SettingsAction::CLIAgents(
                    CLIAgentsPageAction::ToggleAutoOpenRichInputOnCLIAgentStart,
                )),
                &(context.clone() & id!(flags::CLI_AGENT_FOOTER_ENABLED)),
                flags::AUTO_OPEN_RICH_INPUT_ON_CLI_AGENT_START_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| FeatureFlag::CLIAgentRichInput.is_enabled()),
            ToggleSettingActionPair::new(
                "auto dismiss Rich Input after prompt submission",
                builder(SettingsAction::CLIAgents(
                    CLIAgentsPageAction::ToggleAutoDismissRichInputAfterSubmit,
                )),
                &(context.clone() & id!(flags::CLI_AGENT_FOOTER_ENABLED)),
                flags::AUTO_DISMISS_RICH_INPUT_AFTER_SUBMIT_FLAG,
            )
            .with_group(bindings::BindingGroup::WarpAi)
            .with_enabled(|| FeatureFlag::CLIAgentRichInput.is_enabled()),
        ],
        app,
    );
}

/// Widget id backing the `cli_agents` deeplink slug. Lives here alongside the
/// widget itself because the default `widget_id()` is the type's full path,
/// which changes whenever the widget moves modules.
#[cfg(not(target_family = "wasm"))]
pub fn cli_agent_settings_widget_id() -> &'static str {
    CLIAgentWidget::static_widget_id()
}

#[derive(Default)]
struct CLIAgentWidget {
    cli_agent_footer_toggle: SwitchStateHandle,
}

impl SettingsWidget for CLIAgentWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "third party cli coding agent claude codex gemini toolbar footer quick actions show"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let ai_settings = AISettings::as_ref(app);

        let cli_agent_footer_toggle = render_ai_setting_toggle::<ShouldRenderCLIAgentToolbar>(
            "Show coding agent toolbar",
            CLIAgentsPageAction::ToggleCLIAgentToolbar,
            *ai_settings.should_render_cli_agent_footer,
            true,
            self.cli_agent_footer_toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        );

        let description_fragments = vec![
            FormattedTextFragment::plain_text(
                "Show a toolbar with quick actions when running coding agents like ",
            ),
            FormattedTextFragment::inline_code("claude"),
            FormattedTextFragment::plain_text(", "),
            FormattedTextFragment::inline_code("codex"),
            FormattedTextFragment::plain_text(", or "),
            FormattedTextFragment::inline_code("gemini"),
            FormattedTextFragment::plain_text("."),
        ];

        let description = FormattedTextElement::new(
            FormattedText::new([FormattedTextLine::Line(description_fragments)]),
            appearance.ui_font_size(),
            appearance.ui_font_family(),
            appearance.monospace_font_family(),
            styles::description_font_color(true, app).into(),
            HighlightedHyperlink::default(),
        );

        Flex::column()
            .with_child(cli_agent_footer_toggle)
            .with_child(
                Container::new(description.finish())
                    .with_margin_top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                    .with_margin_bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                    .with_margin_right(styles::TOGGLE_WIDTH_MARGIN)
                    .finish(),
            )
            .finish()
    }
}

fn should_render_cli_agent_detail(app: &AppContext) -> bool {
    *AISettings::as_ref(app).should_render_cli_agent_footer
}

fn should_render_cli_agent_rich_input(app: &AppContext) -> bool {
    should_render_cli_agent_detail(app) && FeatureFlag::CLIAgentRichInput.is_enabled()
}

#[derive(Default)]
struct CLIAgentAutoToggleRichInputWidget {
    toggle: SwitchStateHandle,
    info_tooltip: MouseStateHandle,
}

impl SettingsWidget for CLIAgentAutoToggleRichInputWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "third party cli coding agent rich input auto show hide status plugin"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        should_render_cli_agent_rich_input(app)
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

        let label = render_body_item_label::<CLIAgentsPageAction>(
            "Auto show/hide Rich Input based on agent status".into(),
            Some(styles::header_font_color(true, app)),
            Some(AdditionalInfo {
                mouse_state: self.info_tooltip.clone(),
                on_click_action: None,
                secondary_text: None,
                tooltip_override_text: Some(
                    "Requires the Warp plugin for your coding agent".to_owned(),
                ),
            }),
            LocalOnlyIconState::for_setting(
                AutoToggleRichInput::storage_key(),
                AutoToggleRichInput::sync_to_cloud(),
                &mut view.local_only_icon_tooltip_states.borrow_mut(),
                app,
            ),
            ToggleState::Enabled,
            appearance,
        );

        build_toggle_element(
            label,
            render_ai_feature_switch(
                self.toggle.clone(),
                *AISettings::as_ref(app).auto_toggle_rich_input,
                true,
                CLIAgentsPageAction::ToggleAutoToggleRichInput,
                app,
            ),
            appearance,
            None,
        )
    }
}

#[derive(Default)]
struct CLIAgentAutoOpenRichInputWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for CLIAgentAutoOpenRichInputWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "third party cli coding agent rich input auto open session start"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        should_render_cli_agent_rich_input(app)
    }

    fn render(
        &self,
        view: &Self::View,
        _appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        if !self.should_render(app) {
            return Empty::new().finish();
        }

        render_ai_setting_toggle::<AutoOpenRichInputOnCLIAgentStart>(
            "Auto open Rich Input when a coding agent session starts",
            CLIAgentsPageAction::ToggleAutoOpenRichInputOnCLIAgentStart,
            *AISettings::as_ref(app).auto_open_rich_input_on_cli_agent_start,
            true,
            self.toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        )
    }
}

#[derive(Default)]
struct CLIAgentAutoDismissRichInputWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for CLIAgentAutoDismissRichInputWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "third party cli coding agent rich input auto dismiss prompt submission"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        should_render_cli_agent_rich_input(app)
    }

    fn render(
        &self,
        view: &Self::View,
        _appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        if !self.should_render(app) {
            return Empty::new().finish();
        }

        render_ai_setting_toggle::<AutoDismissRichInputAfterSubmit>(
            "Auto dismiss Rich Input after prompt submission",
            CLIAgentsPageAction::ToggleAutoDismissRichInputAfterSubmit,
            *AISettings::as_ref(app).auto_dismiss_rich_input_after_submit,
            true,
            self.toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        )
    }
}

#[derive(Default)]
struct CLIAgentSubmitRichInputWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for CLIAgentSubmitRichInputWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "third party cli coding agent rich input submit ctrl enter newline"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        should_render_cli_agent_rich_input(app)
    }

    fn render(
        &self,
        view: &Self::View,
        _appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        if !self.should_render(app) {
            return Empty::new().finish();
        }

        render_ai_setting_toggle::<SubmitRichInputOnCtrlEnter>(
            "Submit Rich Input with Ctrl+Enter",
            CLIAgentsPageAction::ToggleSubmitRichInputOnCtrlEnter,
            *AISettings::as_ref(app).submit_on_ctrl_enter,
            true,
            self.toggle.clone(),
            &view.local_only_icon_tooltip_states,
            app,
        )
    }
}

struct CLIAgentCommandsWidget;

impl SettingsWidget for CLIAgentCommandsWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "third party cli coding agent claude codex gemini toolbar commands regex patterns"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        should_render_cli_agent_detail(app)
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

        let mut list_column = Flex::column();
        list_column.add_child(
            appearance
                .ui_builder()
                .span("Commands that enable the toolbar".to_string())
                .with_style(UiComponentStyles {
                    font_size: Some(CONTENT_FONT_SIZE),
                    ..Default::default()
                })
                .build()
                .finish(),
        );
        list_column.add_child(ChildView::new(&view.cli_agent_footer_command_editor).finish());

        let background = appearance.theme().surface_1();
        let font_color = appearance.theme().foreground();
        let items: Vec<_> = AISettings::as_ref(app)
            .cli_agent_footer_enabled_commands
            .value()
            .keys()
            .cloned()
            .collect();
        let len = items.len();
        for (rev_i, pattern) in items.iter().rev().enumerate() {
            let original_i = len - 1 - rev_i;
            let remove_action =
                CLIAgentsPageAction::RemoveCLIAgentToolbarEnabledCommand(pattern.clone());
            let mouse_state = view
                .cli_agent_footer_command_mouse_state_handles
                .get(original_i)
                .cloned()
                .unwrap_or_default();

            let remove_button = appearance
                .ui_builder()
                .close_button(16., mouse_state)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(remove_action.clone());
                })
                .finish();

            let label = appearance
                .ui_builder()
                .wrappable_text(pattern.clone(), true)
                .with_style(UiComponentStyles {
                    font_color: Some(font_color.into_solid()),
                    font_family_id: Some(appearance.monospace_font_family()),
                    font_size: Some(appearance.ui_font_size()),
                    ..Default::default()
                })
                .build()
                .finish();

            let mut right_side = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
            if let Some(dropdown_handle) = view
                .cli_agent_footer_command_agent_dropdowns
                .get(original_i)
            {
                right_side.add_child(
                    Container::new(ChildView::new(dropdown_handle).finish())
                        .with_margin_right(8.)
                        .finish(),
                );
            }
            right_side.add_child(remove_button);

            let row = Container::new(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_children([Shrinkable::new(1., label).finish(), right_side.finish()])
                    .finish(),
            )
            .with_background(background)
            .with_horizontal_padding(8.)
            .with_vertical_padding(4.)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
            .with_margin_bottom(4.)
            .finish();

            list_column.add_child(row);
        }

        let description = appearance
            .ui_builder()
            .paragraph("Add regex patterns to show the coding agent toolbar for matching commands.")
            .with_style(UiComponentStyles {
                font_size: Some(appearance.ui_font_size()),
                font_color: Some(styles::description_font_color(true, app).into()),
                margin: Some(
                    Coords::default()
                        .top(4.)
                        .bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                        .right(styles::TOGGLE_WIDTH_MARGIN),
                ),
                ..Default::default()
            })
            .build()
            .finish();

        Flex::column()
            .with_child(list_column.finish())
            .with_child(description)
            .finish()
    }
}

struct CLIAgentToolbarLayoutWidget;

impl SettingsWidget for CLIAgentToolbarLayoutWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "third party cli coding agent toolbar layout chip chips rearrange re-arrange"
    }

    fn should_render(&self, app: &AppContext) -> bool {
        should_render_cli_agent_detail(app) && FeatureFlag::AgentToolbarEditor.is_enabled()
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

        render_toolbar_layout_editor(&view.cli_agent_toolbar_inline_editor, appearance)
    }
}

/// Ward (OSS build): a toggle with a description underneath, for the Ward settings stored in the
/// private user preferences.
fn render_ward_toggle(
    label: &str,
    description: &str,
    is_enabled: bool,
    switch_state: SwitchStateHandle,
    action: CLIAgentsPageAction,
    appearance: &Appearance,
    app: &AppContext,
) -> Box<dyn Element> {
    Flex::column()
        .with_child(build_toggle_element(
            render_body_item_label::<CLIAgentsPageAction>(
                label.to_owned(),
                Some(styles::header_font_color(true, app)),
                None,
                LocalOnlyIconState::Hidden,
                ToggleState::Enabled,
                appearance,
            ),
            render_ai_feature_switch(switch_state, is_enabled, true, action, app),
            appearance,
            None,
        ))
        .with_child(
            appearance
                .ui_builder()
                .paragraph(description.to_owned())
                .with_style(UiComponentStyles {
                    font_size: Some(appearance.ui_font_size()),
                    font_color: Some(styles::description_font_color(true, app).into()),
                    margin: Some(
                        Coords::default()
                            .top(styles::DESCRIPTION_NEGATIVE_MARGIN_OFFSET)
                            .bottom(styles::DESCRIPTION_MARGIN_BOTTOM)
                            .right(styles::TOGGLE_WIDTH_MARGIN),
                    ),
                    ..Default::default()
                })
                .build()
                .finish(),
        )
        .finish()
}

/// Ward (OSS build): the Claude subscription usage chip in the header.
#[derive(Default)]
struct ClaudeUsageChipWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for ClaudeUsageChipWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "claude code subscription usage limit quota 5h 7d weekly chip header ward"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        crate::features::is_fork_oss()
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        if !self.should_render(app) {
            return Empty::new().finish();
        }
        render_ward_toggle(
            "Show Claude subscription usage",
            "Shows the 5-hour and 7-day Claude limits in the header, e.g. Claude 5h 2% \u{b7} 7d 60%. \
             Ward reads the Claude Code sign-in from ~/.claude/.credentials.json every 5 minutes \
             and only sends it to api.anthropic.com. Hidden when Claude Code is not signed in.",
            ward_claude_usage::is_enabled_by_user(app),
            self.toggle.clone(),
            CLIAgentsPageAction::ToggleClaudeUsageChip,
            appearance,
            app,
        )
    }
}

/// Ward (OSS build): the Codex plan usage chip in the header.
#[derive(Default)]
struct CodexUsageChipWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for CodexUsageChipWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "codex openai chatgpt plan usage limit quota 5h 7d weekly chip header ward"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        crate::features::is_fork_oss()
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        if !self.should_render(app) {
            return Empty::new().finish();
        }
        render_ward_toggle(
            "Show Codex plan usage",
            "Shows the Codex limits in the header, e.g. Codex 5h 10% \u{b7} 7d 30%. Ward reads \
             them from the newest Codex CLI session in ~/.codex/sessions, so they are as fresh \
             as the last Codex turn. No network, no credentials. Hidden once the limits reset.",
            ward_codex_usage::is_enabled_by_user(app),
            self.toggle.clone(),
            CLIAgentsPageAction::ToggleCodexUsageChip,
            appearance,
            app,
        )
    }
}

/// Ward (OSS build): Claude Code's fullscreen renderer in the Claude Code tabs Ward starts.
#[derive(Default)]
struct ClaudeNoFlickerWidget {
    toggle: SwitchStateHandle,
}

impl SettingsWidget for ClaudeNoFlickerWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "claude code fullscreen renderer no flicker flickering CLAUDE_CODE_NO_FLICKER ward"
    }

    fn should_render(&self, _app: &AppContext) -> bool {
        crate::features::is_fork_oss()
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        if !self.should_render(app) {
            return Empty::new().finish();
        }
        render_ward_toggle(
            "Claude Code fullscreen renderer (no flicker)",
            "Starts Claude Code tabs opened from the + menu, from Chimney cards and restored \
             sessions with CLAUDE_CODE_NO_FLICKER=1. Applies to tabs opened after the change.",
            ward_claude_launch::is_no_flicker_enabled(app),
            self.toggle.clone(),
            CLIAgentsPageAction::ToggleClaudeNoFlicker,
            appearance,
            app,
        )
    }
}

/// Ward (OSS build): Chimney kanban connection for card chips and starting agents from cards.
#[derive(Default)]
struct ChimneyWidget;

impl SettingsWidget for ChimneyWidget {
    type View = CLIAgentsPageView;

    fn search_terms(&self) -> &str {
        "chimney kanban card board token email password login account ward"
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

        let board_id = ward_chimney::board_id(app);
        let token_source = ward_chimney::token_source(app);
        let email_source = ward_chimney::email_source(app);
        let password_source = ward_chimney::password_source(app);
        let account_status = match (email_source, password_source) {
            (ward_chimney::TokenSource::Missing, _) | (_, ward_chimney::TokenSource::Missing) => {
                "Ward signs in with these, like chimney-mcp, and renews the session itself. \
                 CHIMNEY_EMAIL and CHIMNEY_PASSWORD work as fallbacks."
            }
            (ward_chimney::TokenSource::SecureStorage, ward_chimney::TokenSource::SecureStorage) => {
                "The email and password are saved in the secure storage."
            }
            _ => "Using CHIMNEY_EMAIL / CHIMNEY_PASSWORD from the environment where not saved here.",
        };
        let token_status = match token_source {
            ward_chimney::TokenSource::SecureStorage => {
                "A token is saved in the secure storage; it overrides the service account."
            }
            ward_chimney::TokenSource::Environment => {
                "Using the CHIMNEY_ACCESS_TOKEN environment variable; it overrides the service \
                 account."
            }
            ward_chimney::TokenSource::Missing => {
                "Optional: a ready token (or CHIMNEY_ACCESS_TOKEN) overrides the service account. \
                 It expires and is not renewed."
            }
        };
        let model = ward_chimney::model::model(app);
        let auth_status = model.and_then(|model| match model.auth_status() {
            ward_chimney::auth::AuthStatus::SignedIn { email } => {
                Some(format!("Signed in as {email}."))
            }
            ward_chimney::auth::AuthStatus::Token => {
                Some("Signed in with the access token.".to_owned())
            }
            ward_chimney::auth::AuthStatus::Failed(err) => {
                Some(format!("Last error: {}.", err.message()))
            }
            ward_chimney::auth::AuthStatus::Unknown => None,
        });
        let fetch_status = model.and_then(|model| {
            model
                .error(board_id)
                .filter(|err| {
                    // Sign-in failures are already in the status line.
                    auth_status.is_none()
                        || !matches!(
                            err,
                            ward_chimney::model::FetchError::Auth(_)
                                | ward_chimney::model::FetchError::NotConfigured
                        )
                })
                .map(|err| format!("Last request: {}.", err.message()))
                .or_else(|| {
                    model.board(board_id).map(|board| {
                        format!("Connected to board \u{201c}{}\u{201d}.", board.title)
                    })
                })
        });
        let remove_row = |text: &str, mouse_state: MouseStateHandle, action: CLIAgentsPageAction| {
            let button = appearance
                .ui_builder()
                .close_button(16., mouse_state)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(action.clone());
                })
                .finish();
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(6.)
                .with_child(label(text.to_owned()))
                .with_child(button)
                .finish()
        };

        let mut column = Flex::column()
            .with_child(label("Chimney".to_owned()))
            .with_child(description(format!(
                "Bind tabs to Chimney cards from the tab menu and start agents from cards with \
                 {}. Ward only reads cards and never moves or edits them.",
                crate::util::bindings::keybinding_name_to_display_string(
                    "workspace:ward_start_agent_from_card",
                    app,
                )
                .unwrap_or_else(|| "the command palette".to_owned())
            )))
            .with_child(label(format!("Base URL: {}", ward_chimney::base_url(app))))
            .with_child(ChildView::new(&view.chimney_base_url_editor).finish())
            .with_child(label(format!("Board: {board_id}")))
            .with_child(ChildView::new(&view.chimney_board_editor).finish())
            .with_child(label("Service account".to_owned()))
            .with_child(description(account_status.to_owned()));
        if let Some(email) = ward_chimney::email(app) {
            column.add_child(description(format!("Email: {email}")));
        }
        column.add_child(ChildView::new(&view.chimney_email_editor).finish());
        column.add_child(ChildView::new(&view.chimney_password_editor).finish());
        if email_source == ward_chimney::TokenSource::SecureStorage
            || password_source == ward_chimney::TokenSource::SecureStorage
        {
            column.add_child(remove_row(
                "Remove saved email and password",
                view.chimney_remove_account_mouse_state.clone(),
                CLIAgentsPageAction::RemoveChimneyAccount,
            ));
        }
        column.add_child(label("Access token".to_owned()));
        column.add_child(description(token_status.to_owned()));
        column.add_child(ChildView::new(&view.chimney_token_editor).finish());
        if token_source == ward_chimney::TokenSource::SecureStorage {
            column.add_child(remove_row(
                "Remove saved token",
                view.chimney_remove_token_mouse_state.clone(),
                CLIAgentsPageAction::RemoveChimneyToken,
            ));
        }
        if let Some(auth_status) = auth_status {
            column.add_child(description(auth_status));
        }
        if let Some(fetch_status) = fetch_status {
            column.add_child(description(fetch_status));
        }
        column.finish()
    }
}
