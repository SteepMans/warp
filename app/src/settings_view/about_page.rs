use warpui::assets::asset_cache::AssetSource;
use warpui::elements::{
    Align, CacheOption, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex, Image,
    MainAxisAlignment, MouseStateHandle, ParentElement, Wrap,
};
use warpui::fonts::Weight;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{AppContext, Entity, TypedActionView, View, ViewContext, ViewHandle};

use super::SettingsSection;
use super::settings_page::{
    MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget,
};
use crate::appearance::Appearance;
use crate::channel::ChannelState;
use crate::themes::theme::ColorScheme;
use crate::workspace::WorkspaceAction;

pub struct AboutPageView {
    page: PageType<Self>,
}

impl AboutPageView {
    pub fn new(_ctx: &mut ViewContext<AboutPageView>) -> Self {
        AboutPageView {
            page: PageType::new_monolith(AboutPageWidget::default(), None, false),
        }
    }
}

impl Entity for AboutPageView {
    type Event = SettingsPageEvent;
}
impl TypedActionView for AboutPageView {
    type Action = ();
}

impl View for AboutPageView {
    fn ui_name() -> &'static str {
        "AboutPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

/// Source repository of the fork, shown on the About page of the OSS build.
const FORK_REPO_URL: &str = "https://github.com/SteepMans/warp";

/// What the fork changes compared to upstream Warp.
const FORK_FEATURES: &[&str] = &[
    "Claude Code and Codex tabs are restored after a restart",
    "Autoupdate from the fork's own S3 bucket",
    "No Warp AI, cloud features or Warp account",
];

#[derive(Default)]
struct AboutPageWidget {
    copy_version_button_mouse_state: MouseStateHandle,
    repo_link_mouse_state: MouseStateHandle,
    check_for_update_mouse_state: MouseStateHandle,
    whats_new_mouse_state: MouseStateHandle,
}

impl AboutPageWidget {
    /// Fork details for the OSS build: product name, upstream, repository, update source and
    /// the fork's features. Returns nothing for other channels.
    fn render_fork_info(&self, appearance: &Appearance) -> Vec<Box<dyn Element>> {
        if !crate::features::is_fork_oss() {
            return Vec::new();
        }
        let ui_builder = appearance.ui_builder();
        let sub_text_color = appearance
            .theme()
            .sub_text_color(appearance.theme().surface_2())
            .into_solid();

        let mut children = vec![
            ui_builder
                .span(crate::features::product_name().to_string())
                .with_style(UiComponentStyles {
                    font_size: Some(20.),
                    font_weight: Some(Weight::Bold),
                    ..Default::default()
                })
                .build()
                .with_margin_top(16.)
                .finish(),
            ui_builder
                .span("Fork of Warp (warpdotdev/warp)".to_string())
                .build()
                .with_margin_top(8.)
                .finish(),
            ui_builder
                .link(
                    FORK_REPO_URL.to_string(),
                    Some(FORK_REPO_URL.to_string()),
                    None,
                    self.repo_link_mouse_state.clone(),
                )
                .soft_wrap(false)
                .build()
                .with_margin_top(8.)
                .finish(),
        ];

        let releases_base_url = ChannelState::releases_base_url();
        let releases_base_url = releases_base_url.trim_end_matches('/');
        if !releases_base_url.is_empty() {
            children.push(
                ui_builder
                    .span(format!("Updates: {releases_base_url}/stable/manifest.json"))
                    .with_soft_wrap()
                    .build()
                    .with_margin_top(8.)
                    .finish(),
            );
            children.push(
                ui_builder
                    .link(
                        "Check for updates".to_string(),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(WorkspaceAction::CheckForUpdate);
                        })),
                        self.check_for_update_mouse_state.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .with_margin_top(8.)
                    .finish(),
            );
            children.push(
                ui_builder
                    .link(
                        "What's new".to_string(),
                        None,
                        Some(Box::new(|ctx| {
                            ctx.dispatch_typed_action(WorkspaceAction::ViewLatestChangelog);
                        })),
                        self.whats_new_mouse_state.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .with_margin_top(8.)
                    .finish(),
            );
        }

        children.extend(FORK_FEATURES.iter().enumerate().map(|(index, feature)| {
            ui_builder
                .span(format!("- {feature}"))
                .with_style(UiComponentStyles {
                    font_color: Some(sub_text_color),
                    ..Default::default()
                })
                .with_soft_wrap()
                .build()
                .with_margin_top(if index == 0 { 16. } else { 4. })
                .finish()
        }));

        children
    }
}

impl SettingsWidget for AboutPageWidget {
    type View = AboutPageView;

    fn search_terms(&self) -> &str {
        "about warp ward fork version"
    }

    fn render(
        &self,
        _view: &AboutPageView,
        appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let ui_builder = appearance.ui_builder();

        let image_path = if theme.inferred_color_scheme() == ColorScheme::LightOnDark {
            "bundled/svg/warp-logo-with-light-title.svg"
        } else {
            "bundled/svg/warp-logo-with-dark-title.svg"
        };

        let version = ChannelState::app_version().unwrap_or("v#.##.###");

        let version_text = ui_builder
            .span(version.to_string())
            .with_soft_wrap()
            .build()
            .with_margin_top(16.)
            .finish();

        let copy_version_icon = appearance
            .ui_builder()
            .copy_button(16., self.copy_version_button_mouse_state.clone())
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::CopyVersion(version));
            })
            .finish();

        let version_row = Wrap::row()
            .with_main_axis_alignment(MainAxisAlignment::Center)
            .with_children([
                version_text,
                Container::new(copy_version_icon)
                    .with_margin_top(16.)
                    .with_padding_left(6.)
                    .finish(),
            ]);

        Align::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    ConstrainedBox::new(
                        Image::new(
                            AssetSource::Bundled { path: image_path },
                            CacheOption::BySize,
                        )
                        .finish(),
                    )
                    .with_max_height(100.)
                    .with_max_width(350.)
                    .finish(),
                )
                .with_child(version_row.finish())
                .with_children(self.render_fork_info(appearance))
                .with_child(
                    ui_builder
                        .span("Copyright 2026 Warp")
                        .build()
                        .with_margin_top(16.)
                        .finish(),
                )
                .finish(),
        )
        .finish()
    }
}

impl SettingsPageMeta for AboutPageView {
    fn section() -> SettingsSection {
        SettingsSection::About
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
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

impl From<ViewHandle<AboutPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<AboutPageView>) -> Self {
        SettingsPageViewHandle::About(view_handle)
    }
}
