//! Settings → Updates (Ward only): the running and the latest version, what the updater is
//! doing right now and a "Check now" button that always answers. The logic lives in
//! `crate::ward::updates`; this view renders it and re-renders on every autoupdate change.

use std::time::Duration;

use chrono::Local;
use warpui::r#async::Timer;
use warpui::elements::{
    Container, CrossAxisAlignment, Element, Flex, MainAxisAlignment, MouseStateHandle,
    ParentElement, Wrap,
};
use warpui::fonts::Weight;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Entity, EventContext, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use super::SettingsSection;
use super::settings_page::{
    MatchData, PageType, SettingsPageEvent, SettingsPageMeta, SettingsPageViewHandle,
    SettingsWidget, render_sub_header,
};
use crate::appearance::Appearance;
use crate::autoupdate::{AutoupdateState, fork_manifest};
use crate::channel::ChannelState;
use crate::server::server_api::ServerApiProvider;
use crate::ward::changelog::{self, WardChangelog};
use crate::ward::updates::{self, CheckInfo, CheckNowDecision, StatusAction, StatusKind};
use crate::workspace::WorkspaceAction;

const PAGE_TITLE: &str = "Updates";

/// How often the page re-renders while open, so "Last checked N min ago" stays current.
const REFRESH_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub enum UpdatesPageAction {
    CheckNow,
}

/// Release notes of the version the page describes.
enum Notes {
    Loading,
    Loaded(WardChangelog),
    Failed,
}

pub struct UpdatesPageView {
    page: PageType<Self>,
    /// The outcome of the last "Check now" click, until the update state changes.
    check_now_decision: Option<CheckNowDecision>,
    /// The version whose notes are loaded or loading, and the notes.
    notes: Option<(String, Notes)>,
    /// Whether the page was opened at least once: notes and refreshes only run after that.
    opened: bool,
}

impl UpdatesPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        ctx.observe(&AutoupdateState::handle(ctx), |me, _, ctx| {
            me.check_now_decision = None;
            if me.opened {
                me.load_notes(ctx);
            }
            ctx.notify();
        });
        Self {
            page: PageType::new_monolith(UpdatesWidget::default(), Some(PAGE_TITLE), true),
            check_now_decision: None,
            notes: None,
            opened: false,
        }
    }

    /// Fetches the notes of the latest known version unless they are already loaded.
    fn load_notes(&mut self, ctx: &mut ViewContext<Self>) {
        let latest = AutoupdateState::as_ref(ctx).latest_known_version().cloned();
        let Some(version) = updates::notes_version(latest.as_ref(), ChannelState::app_version())
            .map(ToOwned::to_owned)
        else {
            return;
        };
        if self
            .notes
            .as_ref()
            .is_some_and(|(loaded, notes)| *loaded == version && !matches!(notes, Notes::Failed))
        {
            return;
        }
        self.notes = Some((version.clone(), Notes::Loading));
        let server_api = ServerApiProvider::as_ref(ctx).get();
        let requested = version.clone();
        ctx.spawn(
            async move { changelog::fetch_for_version(server_api.http_client(), &requested).await },
            move |me, result, ctx| {
                if !me
                    .notes
                    .as_ref()
                    .is_some_and(|(loading, _)| *loading == version)
                {
                    return;
                }
                let notes = match result {
                    Ok(changelog) => Notes::Loaded(changelog),
                    Err(err) => {
                        log::info!("No Ward changelog for {version}: {err:#}");
                        Notes::Failed
                    }
                };
                me.notes = Some((version, notes));
                ctx.notify();
            },
        );
    }

    fn schedule_refresh(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.spawn(
            async {
                Timer::after(REFRESH_INTERVAL).await;
            },
            |me, _, ctx| {
                me.check_now_decision = None;
                ctx.notify();
                me.schedule_refresh(ctx);
            },
        );
    }
}

impl Entity for UpdatesPageView {
    type Event = SettingsPageEvent;
}

impl TypedActionView for UpdatesPageView {
    type Action = UpdatesPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            UpdatesPageAction::CheckNow => {
                let decision = updates::check_now(ctx);
                // A started check changes the state, which clears the note again.
                self.check_now_decision = Some(decision);
                ctx.notify();
            }
        }
    }
}

impl View for UpdatesPageView {
    fn ui_name() -> &'static str {
        "UpdatesPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Default)]
struct UpdatesWidget {
    action_button: MouseStateHandle,
    check_now_button: MouseStateHandle,
    copy_version_button: MouseStateHandle,
    whats_new_link: MouseStateHandle,
    all_changes_link: MouseStateHandle,
}

impl UpdatesWidget {
    fn text(
        appearance: &Appearance,
        text: impl Into<String>,
        styles: UiComponentStyles,
    ) -> Box<dyn Element> {
        appearance
            .ui_builder()
            .span(text.into())
            .with_style(styles)
            .with_soft_wrap()
            .build()
            .finish()
    }

    fn sub_text(appearance: &Appearance, text: impl Into<String>) -> Box<dyn Element> {
        let theme = appearance.theme();
        Self::text(
            appearance,
            text,
            UiComponentStyles {
                font_color: Some(theme.sub_text_color(theme.surface_2()).into_solid()),
                ..Default::default()
            },
        )
    }

    fn button(
        appearance: &Appearance,
        label: &str,
        variant: ButtonVariant,
        mouse_state: MouseStateHandle,
        on_click: impl Fn(&mut EventContext) + 'static,
    ) -> Box<dyn Element> {
        Container::new(
            appearance
                .ui_builder()
                .button(variant, mouse_state)
                .with_text_label(label.to_owned())
                .with_style(UiComponentStyles {
                    font_size: Some(13.),
                    padding: Some(Coords::uniform(6.).left(16.).right(16.)),
                    ..Default::default()
                })
                .build()
                .on_click(move |ctx, _, _| on_click(ctx))
                .finish(),
        )
        .with_margin_right(8.)
        .finish()
    }

    fn render_status(
        &self,
        view: &UpdatesPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Vec<Box<dyn Element>> {
        let theme = appearance.theme();
        let state = AutoupdateState::as_ref(app);
        let latest = state.latest_known_version();
        let status = updates::status(
            state.stage(),
            CheckInfo {
                checked: state.last_check_finished_at().is_some(),
                error: state.last_check_error(),
                latest_version: latest.map(|version| version.version.as_str()),
                polling_started: state.is_polling_started(),
            },
        );
        let color = match status.kind {
            StatusKind::Busy | StatusKind::Idle => theme.active_ui_text_color().into_solid(),
            StatusKind::Ready => theme.ui_green_color(),
            StatusKind::Error => theme.ui_error_color(),
        };
        let mut children = vec![Self::text(
            appearance,
            status.text,
            UiComponentStyles {
                font_size: Some(16.),
                font_weight: Some(Weight::Semibold),
                font_color: Some(color),
                ..Default::default()
            },
        )];

        let mut buttons = Flex::row().with_main_axis_alignment(MainAxisAlignment::Start);
        if let Some(action) = status.action {
            let on_click: Box<dyn Fn(&mut EventContext)> = match action {
                StatusAction::InstallAndRestart | StatusAction::Restart => Box::new(|ctx| {
                    ctx.dispatch_typed_action(WorkspaceAction::ApplyUpdate);
                }),
                StatusAction::Retry => Box::new(|ctx| {
                    ctx.dispatch_typed_action(UpdatesPageAction::CheckNow);
                }),
            };
            buttons.add_child(Self::button(
                appearance,
                action.label(),
                ButtonVariant::Accent,
                self.action_button.clone(),
                on_click,
            ));
        }
        if status.action != Some(StatusAction::Retry) {
            buttons.add_child(Self::button(
                appearance,
                "Check now",
                ButtonVariant::Secondary,
                self.check_now_button.clone(),
                |ctx| ctx.dispatch_typed_action(UpdatesPageAction::CheckNow),
            ));
        }
        children.push(
            Container::new(buttons.finish())
                .with_margin_top(12.)
                .finish(),
        );

        if let Some(note) = view
            .check_now_decision
            .and_then(|decision| decision.note(state.is_polling_started()))
        {
            children.push(
                Container::new(Self::sub_text(appearance, note))
                    .with_margin_top(8.)
                    .finish(),
            );
        }
        children.push(
            Container::new(Self::sub_text(
                appearance,
                updates::last_checked_text(state.last_check_finished_at(), Local::now()),
            ))
            .with_margin_top(8.)
            .finish(),
        );
        children
    }

    fn render_versions(&self, appearance: &Appearance, app: &AppContext) -> Vec<Box<dyn Element>> {
        let ui_builder = appearance.ui_builder();
        let state = AutoupdateState::as_ref(app);
        let current = ChannelState::app_version().unwrap_or("unknown");

        let copy_button = ui_builder
            .copy_button(14., self.copy_version_button.clone())
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::CopyVersion(current));
            })
            .finish();
        let mut children = vec![
            Wrap::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_children([
                    Self::text(
                        appearance,
                        format!("Installed: {current}"),
                        Default::default(),
                    ),
                    Container::new(copy_button).with_padding_left(6.).finish(),
                ])
                .finish(),
        ];
        let latest = match state.latest_known_version() {
            Some(latest) if latest.version == current => format!("Latest: {current} (installed)"),
            Some(latest) => format!("Latest: {}", latest.version),
            None => "Latest: unknown until the first check".to_owned(),
        };
        children.push(
            Container::new(Self::text(appearance, latest, Default::default()))
                .with_margin_top(6.)
                .finish(),
        );
        children.push(
            Container::new(Self::sub_text(
                appearance,
                format!("Channel: {}", ChannelState::channel()),
            ))
            .with_margin_top(6.)
            .finish(),
        );
        let source = fork_manifest::stable_dir_url()
            .map(|dir| format!("Update source: {dir}/manifest.json"))
            .unwrap_or_else(|| "Update source: none configured in this build".to_owned());
        children.push(
            Container::new(Self::sub_text(appearance, source))
                .with_margin_top(4.)
                .finish(),
        );
        children
    }

    fn render_notes(
        &self,
        view: &UpdatesPageView,
        appearance: &Appearance,
    ) -> Vec<Box<dyn Element>> {
        let ui_builder = appearance.ui_builder();
        let mut children = Vec::new();
        match &view.notes {
            Some((version, notes)) => {
                children.push(render_sub_header(
                    appearance,
                    format!("What's new in {version}"),
                    None,
                ));
                match notes {
                    Notes::Loading => children.push(Self::sub_text(appearance, "Loading…")),
                    Notes::Failed => children.push(Self::sub_text(
                        appearance,
                        "No release notes published for this version",
                    )),
                    Notes::Loaded(changelog) => {
                        if let Some(details) = updates::release_details(changelog) {
                            children.push(Self::sub_text(appearance, details));
                        }
                        let (lines, hidden) = updates::short_notes(changelog);
                        if lines.is_empty() {
                            children.push(Self::sub_text(appearance, "No notable changes"));
                        }
                        for line in lines {
                            let title = if line.title.is_empty() {
                                line.section.to_owned()
                            } else {
                                format!("{} · {}", line.section, line.title)
                            };
                            children.push(
                                Container::new(Self::text(
                                    appearance,
                                    title,
                                    UiComponentStyles {
                                        font_weight: Some(Weight::Semibold),
                                        ..Default::default()
                                    },
                                ))
                                .with_margin_top(10.)
                                .finish(),
                            );
                            if !line.text.is_empty() {
                                children.push(
                                    Container::new(Self::sub_text(appearance, line.text))
                                        .with_margin_top(2.)
                                        .finish(),
                                );
                            }
                        }
                        if hidden > 0 {
                            children.push(
                                Container::new(Self::sub_text(
                                    appearance,
                                    format!("…and {hidden} more"),
                                ))
                                .with_margin_top(8.)
                                .finish(),
                            );
                        }
                    }
                }
            }
            None => children.push(render_sub_header(appearance, "What's new", None)),
        }

        let links = Flex::row()
            .with_child(
                Container::new(
                    ui_builder
                        .link(
                            "Open What's new for the installed version".to_owned(),
                            None,
                            Some(Box::new(|ctx| {
                                ctx.dispatch_typed_action(WorkspaceAction::ViewLatestChangelog);
                            })),
                            self.whats_new_link.clone(),
                        )
                        .soft_wrap(false)
                        .build()
                        .finish(),
                )
                .with_margin_right(16.)
                .finish(),
            )
            .with_child(
                ui_builder
                    .link(
                        "All changes".to_owned(),
                        Some(changelog::ALL_CHANGES_URL.to_owned()),
                        None,
                        self.all_changes_link.clone(),
                    )
                    .soft_wrap(false)
                    .build()
                    .finish(),
            );
        children.push(Container::new(links.finish()).with_margin_top(12.).finish());
        children
    }
}

impl SettingsWidget for UpdatesWidget {
    type View = UpdatesPageView;

    fn search_terms(&self) -> &str {
        "updates update check version install restart what's new changelog ward"
    }

    fn render(
        &self,
        view: &UpdatesPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let section = |children: Vec<Box<dyn Element>>, margin_top: f32| {
            Container::new(
                Flex::column()
                    .with_cross_axis_alignment(CrossAxisAlignment::Start)
                    .with_children(children)
                    .finish(),
            )
            .with_margin_top(margin_top)
            .finish()
        };
        let mut versions = vec![render_sub_header(appearance, "Version", None)];
        versions.extend(self.render_versions(appearance, app));
        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(section(self.render_status(view, appearance, app), 8.))
            .with_child(section(versions, 28.))
            .with_child(section(self.render_notes(view, appearance), 28.))
            .finish()
    }
}

impl SettingsPageMeta for UpdatesPageView {
    fn section() -> SettingsSection {
        SettingsSection::Updates
    }

    fn on_page_selected(&mut self, _allow_steal_focus: bool, ctx: &mut ViewContext<Self>) {
        if !self.opened {
            self.opened = true;
            self.schedule_refresh(ctx);
        }
        self.load_notes(ctx);
        ctx.notify();
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

impl From<ViewHandle<UpdatesPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<UpdatesPageView>) -> Self {
        SettingsPageViewHandle::Updates(view_handle)
    }
}
