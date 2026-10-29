//! Ward (OSS build) Chimney flows of the workspace: binding a tab to a card from the tab menu
//! and starting a CLI agent from a card (Ctrl+Alt+N). Both pick the card in the command
//! palette's Chimney card mode.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use warpui::platform::FilePickerConfiguration;
use warpui::{AppContext, EntityId, ViewContext};

use super::Workspace;
use crate::palette::PaletteMode;
use crate::pane_group::{NewTerminalOptions, PanesLayout};
use crate::search::QueryFilter;
use crate::server::telemetry::PaletteSource;
use crate::ward::chimney::api::{CardChoice, picker_cards};
use crate::ward::chimney::bindings::{self, CardRef};
use crate::ward::chimney::picker::{self, PickPurpose};
use crate::ward::chimney::{self, repos, slug};

impl Workspace {
    /// Terminal view and persistent pane uuid (hex) of the tab's active terminal pane.
    fn ward_tab_terminal_pane(&self, tab_index: usize, app: &AppContext) -> Option<(EntityId, String)> {
        let pane_group = self.get_pane_group_view(tab_index)?.as_ref(app);
        let terminal_view = pane_group.active_session_view(app)?;
        let uuid = pane_group.terminal_pane_session_uuid(terminal_view.id(), app)?;
        Some((terminal_view.id(), bindings::uuid_hex(&uuid)))
    }

    pub(super) fn ward_bind_chimney_card(&mut self, tab_index: usize, ctx: &mut ViewContext<Self>) {
        let Some((terminal_view_id, pane_uuid)) = self.ward_tab_terminal_pane(tab_index, ctx) else {
            return;
        };
        bindings::register_view(terminal_view_id, pane_uuid.clone());
        self.ward_open_chimney_picker(PickPurpose::Bind { pane_uuid }, ctx);
    }

    pub(super) fn ward_unbind_chimney_card(&mut self, tab_index: usize, ctx: &mut ViewContext<Self>) {
        if let Some((_, pane_uuid)) = self.ward_tab_terminal_pane(tab_index, ctx) {
            bindings::unbind(&pane_uuid, ctx);
            ctx.notify();
        }
    }

    pub(super) fn ward_start_agent_from_card(&mut self, ctx: &mut ViewContext<Self>) {
        self.ward_open_chimney_picker(PickPurpose::StartAgent, ctx);
    }

    /// Opens the command palette in the card mode. The board is fetched again so the picker
    /// shows current columns; the list fills in when the fetch lands.
    fn ward_open_chimney_picker(&mut self, purpose: PickPurpose, ctx: &mut ViewContext<Self>) {
        picker::set_pending(purpose);
        chimney::model::refresh_board(chimney::board_id(ctx), ctx);
        self.open_palette_action(PaletteMode::Command, PaletteSource::Keybinding, None, ctx);
        self.palette.update(ctx, |palette, ctx| {
            palette.set_active_query_filter(QueryFilter::WardChimneyCards, ctx);
        });
    }

    /// Re-runs the picker query when a board fetch lands while the picker is open.
    pub(super) fn ward_refresh_chimney_picker(&mut self, ctx: &mut ViewContext<Self>) {
        if picker::pending().is_none() || !self.current_workspace_state.is_palette_open {
            return;
        }
        self.palette.update(ctx, |palette, ctx| {
            palette
                .search_bar
                .update(ctx, |search_bar, ctx| search_bar.run_query(ctx));
        });
    }

    pub(super) fn ward_chimney_card_picked(
        &mut self,
        board_id: u64,
        task_id: u64,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(purpose) = picker::take_pending() else {
            return;
        };
        match purpose {
            PickPurpose::Bind { pane_uuid } => {
                bindings::bind(&pane_uuid, CardRef { board_id, task_id }, ctx);
                chimney::model::ensure_polling(ctx);
                ctx.notify();
            }
            PickPurpose::StartAgent => {
                let Some(card) = chimney::model::model(ctx)
                    .and_then(|model| model.board(board_id))
                    .and_then(|board| {
                        picker_cards(board, true)
                            .into_iter()
                            .find(|card| card.task_id == task_id)
                    })
                else {
                    return;
                };
                match repos::repo_for_labels(&card.labels, ctx) {
                    Some(repo) => {
                        self.ward_launch_card_agent(card, Some(repo.label), repo.path, ctx)
                    }
                    None => self.ward_pick_repo_for_card(card, ctx),
                }
            }
        }
    }

    /// No repository is known for the card's project: asks for a directory and remembers it
    /// for the card's first label.
    fn ward_pick_repo_for_card(&mut self, card: CardChoice, ctx: &mut ViewContext<Self>) {
        let workspace = ctx.handle();
        ctx.open_file_picker(
            move |result, ctx| {
                let Ok(paths) = result else { return };
                let Some(path) = paths.into_iter().next() else {
                    return;
                };
                let label = card.labels.first().cloned();
                if let Some(label) = &label {
                    repos::remember(label, &path, ctx);
                }
                let Some(workspace) = workspace.upgrade(ctx) else {
                    return;
                };
                workspace.update(ctx, |workspace, ctx| {
                    workspace.ward_launch_card_agent(card, label, path, ctx);
                });
            },
            FilePickerConfiguration::new().folders_only(),
        );
    }

    /// New tab in the repository running `claude --remote-control '<project>. <task-slug>'`,
    /// bound to the card.
    fn ward_launch_card_agent(
        &mut self,
        card: CardChoice,
        label: Option<String>,
        path: String,
        ctx: &mut ViewContext<Self>,
    ) {
        let project = repos::session_project(label.as_deref(), &path);
        let command = slug::claude_command(&project, &slug::task_slug(&card.title, card.task_id));
        self.add_tab_with_pane_layout(
            PanesLayout::SingleTerminal(Box::new(NewTerminalOptions {
                initial_directory: Some(PathBuf::from(&path)),
                hide_homepage: true,
                ..Default::default()
            })),
            Arc::new(HashMap::new()),
            None,
            ctx,
        );
        let Some(terminal_view) = self.active_session_view(ctx) else {
            return;
        };
        let uuid = self
            .active_tab_pane_group()
            .as_ref(ctx)
            .terminal_pane_session_uuid(terminal_view.id(), ctx);
        if let Some(uuid) = uuid {
            let pane_uuid = bindings::uuid_hex(&uuid);
            bindings::register_view(terminal_view.id(), pane_uuid.clone());
            bindings::bind(
                &pane_uuid,
                CardRef {
                    board_id: card.board_id,
                    task_id: card.task_id,
                },
                ctx,
            );
            chimney::model::ensure_polling(ctx);
        }
        terminal_view.update(ctx, |view, ctx| {
            view.set_pending_command_queue(vec![command], ctx);
        });
        ctx.notify();
    }
}
