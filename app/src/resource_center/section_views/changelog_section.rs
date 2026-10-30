use instant::Instant;
use markdown_parser::{FormattedText, FormattedTextFragment, FormattedTextLine};
use warp_core::features::FeatureFlag;
use warpui::elements::{
    Border, CacheOption, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element,
    Flex, FormattedTextElement, HighlightedHyperlink, Icon, Image, MouseStateHandle, ParentElement,
    Radius,
};
use warpui::fonts::Weight;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Entity, ModelAsRef, ModelHandle, SingletonEntity, TypedActionView, View,
    ViewContext,
};

use super::feature_section::FeatureSection;
use super::{SectionAction, SectionView};
use crate::appearance::Appearance;
use crate::changelog_model::{
    ChangelogHeader, ChangelogModel, ChangelogState, Event as ChangelogEvent,
};
use crate::send_telemetry_from_ctx;
use crate::server::telemetry::TelemetryEvent;
use crate::themes::theme::Fill;
use crate::ui_components::icons;

#[derive(Default)]
struct ChangelogMouseStateHandles {
    top_bar_mouse_state: MouseStateHandle,
    view_changelogs_mouse_state: MouseStateHandle,
}

const CHANGELOG_FETCH_ERROR_MSG: &str = "Unable to fetch the latest changelog.";
const CHANGELOG_LOADING_MSG: &str = "Loading...";

pub struct ChangelogSectionView {
    changelog_model_handle: ModelHandle<ChangelogModel>,
    changelog_button_mouse_states: ChangelogMouseStateHandles,
    is_expanded: bool,
    // If showing changelog after app update, show special "New features" header to draw attention
    show_special_new_features_header: bool,
    new_features_highlighted_link: HighlightedHyperlink,
    improvements_highlighted_link: HighlightedHyperlink,
    bug_fixes_highlighted_link: HighlightedHyperlink,
    changelog_fetch_error: FormattedText,
    changelog_loading: FormattedText,
}

impl Entity for ChangelogSectionView {
    type Event = ();
}

impl TypedActionView for ChangelogSectionView {
    type Action = SectionAction;

    fn handle_action(&mut self, action: &SectionAction, ctx: &mut ViewContext<Self>) {
        use SectionAction::*;
        match action {
            OpenUrl(url) => {
                send_telemetry_from_ctx!(
                    TelemetryEvent::OpenChangelogLink { url: url.clone() },
                    ctx
                );
                ctx.open_url(url.as_str());
            }
            ToggleExpanded => self.toggle_expanded(ctx),
            _ => {}
        }
    }
}

fn create_formatted_text_from_string(message: String) -> FormattedText {
    FormattedText {
        lines: vec![FormattedTextLine::Line(vec![
            FormattedTextFragment::plain_text(message),
        ])]
        .into(),
    }
}

impl ChangelogSectionView {
    pub fn new(
        changelog_model_handle: ModelHandle<ChangelogModel>,
        showing_new_changelog: bool,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        ctx.subscribe_to_model(&changelog_model_handle, |me, _, event, ctx| {
            me.handle_changelog_event(event, ctx);
        });

        Self {
            changelog_model_handle,
            changelog_button_mouse_states: Default::default(),
            is_expanded: showing_new_changelog,
            show_special_new_features_header: showing_new_changelog,
            new_features_highlighted_link: Default::default(),
            improvements_highlighted_link: Default::default(),
            bug_fixes_highlighted_link: Default::default(),
            changelog_fetch_error: create_formatted_text_from_string(
                CHANGELOG_FETCH_ERROR_MSG.to_string(),
            ),
            changelog_loading: create_formatted_text_from_string(CHANGELOG_LOADING_MSG.to_string()),
        }
    }

    fn handle_changelog_event(&mut self, _: &ChangelogEvent, ctx: &mut ViewContext<Self>) {
        ctx.notify();
    }

    /// Generate all of the supported changelog sections and add them to the content
    ///
    /// The supported sections are, in order:
    ///
    /// * New features
    /// * Improvements
    /// * Bug fixes
    ///
    /// The first section present gets the special header after an update and the image (if
    /// available), so a release without new features still shows both.
    fn generate_changelog_sections(
        &self,
        content: &mut Flex,
        model: &ChangelogModel,
        appearance: &Appearance,
    ) {
        let sections = [
            (
                ChangelogHeader::NewFeatures,
                icons::Icon::Gift,
                self.new_features_highlighted_link.clone(),
            ),
            (
                ChangelogHeader::Improvements,
                icons::Icon::Tool,
                self.improvements_highlighted_link.clone(),
            ),
            (
                ChangelogHeader::BugFixes,
                icons::Icon::Bug,
                self.bug_fixes_highlighted_link.clone(),
            ),
        ];

        let mut is_first = true;
        for (section, icon, link) in sections {
            let title = section.to_string();
            let Some(markdown) = model.parsed_changelog.get(&title) else {
                continue;
            };

            // Title
            if is_first && self.show_special_new_features_header {
                content.add_child(render_special_changelog_header(
                    &title,
                    render_icon(icon, appearance.theme().terminal_colors().normal.red.into()),
                    appearance,
                ));
            } else {
                content.add_child(render_basic_changelog_header(
                    &title,
                    render_icon(
                        icon,
                        appearance
                            .theme()
                            .sub_text_color(appearance.theme().surface_2()),
                    ),
                    appearance,
                ));
            }

            // Image (if available)
            if is_first && let Some(image_source) = &model.image {
                content.add_child(
                    Container::new(
                        ConstrainedBox::new(
                            Image::new(image_source.clone(), CacheOption::BySize)
                                .enable_animation_with_start_time(Instant::now())
                                .finish(),
                        )
                        .with_max_height(200.)
                        .with_max_width(350.)
                        .finish(),
                    )
                    .with_margin_top(4.)
                    .finish(),
                );
            }
            is_first = false;

            // Content
            content.add_child(render_changelog_body(markdown.clone(), link, appearance));
        }
    }
}

fn render_icon(icon: icons::Icon, color: Fill) -> ConstrainedBox {
    ConstrainedBox::new(Icon::new(icon.into(), color).finish())
        .with_width(16.)
        .with_height(16.)
}

fn render_special_changelog_header(
    title: &str,
    icon: ConstrainedBox,
    appearance: &Appearance,
) -> Box<dyn Element> {
    Container::new(
        Container::new(
            Flex::row()
                .with_child(icon.finish())
                .with_child(
                    Container::new(
                        appearance
                            .ui_builder()
                            .span(title.to_uppercase())
                            .with_style(UiComponentStyles {
                                font_color: Some(appearance.theme().failed_block_color().into()),
                                font_weight: Some(Weight::Bold),
                                font_size: Some(16.0),
                                ..Default::default()
                            })
                            .build()
                            .finish(),
                    )
                    .with_margin_left(8.)
                    .finish(),
                )
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .finish(),
        )
        .with_horizontal_padding(12.)
        .with_vertical_padding(4.)
        .with_border(
            Border::all(1.0)
                .with_border_fill::<Fill>(appearance.theme().terminal_colors().normal.red.into()),
        )
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
        .finish(),
    )
    .with_margin_top(12.)
    .with_margin_right(158.)
    .with_margin_left(16.)
    .with_margin_bottom(4.)
    .finish()
}

fn render_basic_changelog_header(
    title: &str,
    icon: ConstrainedBox,
    appearance: &Appearance,
) -> Box<dyn Element> {
    Container::new(
        Flex::row()
            .with_child(icon.finish())
            .with_child(
                Container::new(
                    appearance
                        .ui_builder()
                        .span(title.to_string())
                        .with_style(UiComponentStyles {
                            font_color: Some(
                                appearance
                                    .theme()
                                    .sub_text_color(appearance.theme().surface_2())
                                    .into(),
                            ),
                            font_weight: Some(Weight::Normal),
                            font_size: Some(16.0),
                            ..Default::default()
                        })
                        .build()
                        .finish(),
                )
                .with_margin_left(8.)
                .finish(),
            )
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .finish(),
    )
    .with_margin_top(12.)
    .with_margin_right(16.)
    .with_margin_left(16.)
    .with_margin_bottom(4.)
    .finish()
}

fn render_changelog_body(
    parsed_markdown: FormattedText,
    highlighted_link: HighlightedHyperlink,
    appearance: &Appearance,
) -> Box<dyn Element> {
    Container::new(
        FormattedTextElement::new(
            parsed_markdown,
            14.0,
            appearance.ui_font_family(),
            appearance.monospace_font_family(),
            appearance
                .theme()
                .main_text_color(appearance.theme().surface_2())
                .into_solid(),
            highlighted_link,
        )
        .register_default_click_handlers(move |url, ctx, _| {
            ctx.dispatch_typed_action(SectionAction::OpenUrl(url.url));
        })
        .finish(),
    )
    .with_margin_top(12.)
    .with_margin_right(16.)
    .with_margin_left(16.)
    .with_margin_bottom(12.)
    .finish()
}

impl SectionView for ChangelogSectionView {
    fn is_expanded(&self) -> bool {
        self.is_expanded
    }

    fn toggle_expanded(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_expanded = !self.is_expanded;
        ctx.notify();
    }

    fn section_progress_indicator(
        &self,
        _show_gamified: bool,
        _appearance: &Appearance,
        _ctx: &AppContext,
    ) -> Option<Box<dyn Element>> {
        None
    }

    fn section_link(&self, appearance: &Appearance) -> Option<Box<dyn Element>> {
        Some(
            appearance
                .ui_builder()
                .link(
                    "Read all changelogs".into(),
                    Some(if crate::features::is_fork_oss() {
                        crate::ward::changelog::ALL_CHANGES_URL.into()
                    } else {
                        "https://docs.warp.dev/changelog".into()
                    }),
                    None,
                    self.changelog_button_mouse_states
                        .view_changelogs_mouse_state
                        .clone(),
                )
                .soft_wrap(false)
                .with_style(UiComponentStyles {
                    border_width: Some(2.),
                    font_size: Some(14.0),
                    font_weight: Some(Weight::Normal),
                    ..Default::default()
                })
                .build()
                .finish(),
        )
    }
}

impl View for ChangelogSectionView {
    fn ui_name() -> &'static str {
        "ResourceCenterChangelogSectionView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let changelog_model = app.model(&self.changelog_model_handle);
        let appearance = Appearance::as_ref(app);
        let header = self.render_section_header(
            FeatureSection::WhatsNew,
            false,
            appearance,
            self.changelog_button_mouse_states
                .top_bar_mouse_state
                .clone(),
            app,
        );

        let mut section = Flex::column().with_child(header);

        if self.is_expanded || FeatureFlag::AvatarInTabBar.is_enabled() {
            let mut content_flex = Flex::column();
            match &changelog_model.changelog {
                ChangelogState::Some(_) => {
                    self.generate_changelog_sections(
                        &mut content_flex,
                        changelog_model,
                        appearance,
                    );
                }
                ChangelogState::Pending => {
                    content_flex.add_child(render_changelog_body(
                        self.changelog_loading.clone(),
                        self.new_features_highlighted_link.clone(),
                        appearance,
                    ));
                }
                ChangelogState::None => {
                    content_flex.add_child(render_changelog_body(
                        self.changelog_fetch_error.clone(),
                        self.new_features_highlighted_link.clone(),
                        appearance,
                    ));
                }
            }

            let content_section = Container::new(content_flex.finish())
                .with_margin_top(4.)
                .with_margin_bottom(4.);
            section.add_child(content_section.finish());
        }

        section.finish()
    }
}
