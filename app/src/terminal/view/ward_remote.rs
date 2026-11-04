//! Ward (OSS build) hooks of the terminal for agent sessions watched from another machine.
//! Kept out of `view.rs` so the upstream file only carries small calls.
//!
//! On the machine of a session, its tab takes the size of the viewer in Ward on another machine
//! for as long as the stream lasts (`crate::ward::agents::stream`). It goes through the same
//! path as Warp's viewer-driven sizing of shared sessions: the pane keeps its pixels, the grid
//! and the PTY get the viewer's columns and rows, and a later layout does not undo it.
//!
//! On the machine of the viewer, the tab of a session elsewhere (`crate::ward::agents::remote_tab`)
//! hides the command that runs the viewer and the input box while the viewer runs, and its pane
//! header says it is remote control and of which machine.

#[cfg(not(target_arch = "wasm32"))]
use session_sharing_protocol::common::WindowSize;
use warpui::elements::{
    ConstrainedBox, Container, CrossAxisAlignment, Flex, MainAxisSize, ParentElement, Shrinkable,
    Text,
};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, Element, SingletonEntity as _, ViewContext};

use super::TerminalView;
use crate::appearance::Appearance;
use crate::terminal::model::TerminalModel;
use crate::terminal::model::block::BlockId;
use crate::ui_components::icons::Icon;
use crate::ward::agents::remote_tab::{self, Phase, RemoteTab};

impl TerminalView {
    /// Gives the PTY of this terminal `cols` x `rows` of a viewer on another machine, or the
    /// size of its pane back with `None`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn ward_set_remote_viewer_size(
        &mut self,
        size: Option<(usize, usize)>,
        ctx: &mut ViewContext<Self>,
    ) {
        match size {
            Some((cols, rows)) => self.resize_from_viewer_report(
                WindowSize {
                    num_rows: rows,
                    num_cols: cols,
                },
                ctx,
            ),
            None => {
                if self.active_viewer_driven_size.is_some() {
                    self.restore_pty_to_sharer_size(ctx);
                }
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    pub fn ward_set_remote_viewer_size(
        &mut self,
        _size: Option<(usize, usize)>,
        _ctx: &mut ViewContext<Self>,
    ) {
    }

    /// The remote tab this terminal is, with where its viewer is; `None` for any other terminal.
    pub fn ward_remote_tab(&self) -> Option<(RemoteTab, Phase)> {
        let tab = remote_tab::of_view(self.view_id)?;
        let phase = ward_remote_phase(&tab, &self.model.lock());
        Some((tab, phase))
    }

    /// The tab was marked as remote, or its viewer started or ended: the header follows.
    pub fn ward_remote_tab_changed(&mut self, ctx: &mut ViewContext<Self>) {
        self.pane_configuration.update(ctx, |config, ctx| {
            config.notify_header_content_changed(ctx);
        });
        ctx.notify();
    }

    /// Whether the input box of this terminal is hidden for a remote tab: until its viewer is
    /// over it would only show the command that runs the viewer. Takes the model already
    /// locked by [`TerminalView::is_input_box_visible`].
    pub(super) fn ward_remote_hides_input(&self, model: &TerminalModel) -> bool {
        remote_tab::of_view(self.view_id)
            .is_some_and(|tab| ward_remote_phase(&tab, model) != Phase::Ended)
    }

    /// A command block started. The first one of a remote tab runs the viewer: its command
    /// line is hidden, the output of the session is all the block shows.
    pub(super) fn ward_remote_block_started(
        &mut self,
        block_id: &BlockId,
        ctx: &mut ViewContext<Self>,
    ) {
        if !remote_tab::claim_block(self.view_id, block_id.as_str()) {
            return;
        }
        {
            let mut model = self.model.lock();
            let block_list = model.block_list_mut();
            if let Some(block) = block_list.mut_block_from_id(block_id) {
                block.set_should_hide_command_grid(true);
            }
            block_list.update_active_block_height();
        }
        self.ward_remote_tab_changed(ctx);
    }

    /// A command block finished: the header of a remote tab whose viewer is over says so.
    pub(super) fn ward_remote_block_completed(&mut self, ctx: &mut ViewContext<Self>) {
        if remote_tab::is_remote(self.view_id) {
            self.ward_remote_tab_changed(ctx);
        }
    }

    /// Whether the pane header is shown for a remote tab: always, it says what the tab is.
    pub(super) fn ward_renders_remote_header(&self) -> bool {
        remote_tab::is_remote(self.view_id)
    }

    /// The title of the pane header of a remote tab: the remote icon, the machine and the task,
    /// and how to disconnect. `is_dragging` keeps every child rigid while the pane is dragged.
    pub(super) fn ward_render_remote_header_title(
        &self,
        is_dragging: bool,
        app: &AppContext,
    ) -> Option<Box<dyn Element>> {
        let (tab, phase) = self.ward_remote_tab()?;
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let sub_text = theme.sub_text_color(theme.background()).into_solid();
        let (accent, text_color) = if phase == Phase::Ended {
            (sub_text, sub_text)
        } else {
            (
                theme.ansi_fg_magenta(),
                theme.main_text_color(theme.background()).into_solid(),
            )
        };

        let icon = ConstrainedBox::new(Icon::Laptop.to_warpui_icon(accent.into()).finish())
            .with_width(font_size)
            .with_height(font_size)
            .finish();
        let title = Text::new_inline(tab.header(phase), font_family, font_size)
            .with_color(text_color)
            .with_clip(ClipConfig::ellipsis())
            .finish();
        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(Container::new(icon).with_margin_right(6.).finish());
        if is_dragging {
            row.add_child(title);
        } else {
            row.add_child(Shrinkable::new(1., title).finish());
        }
        if let Some(hint) = RemoteTab::hint(phase) {
            row.add_child(
                Container::new(
                    Text::new_inline(hint, font_family, font_size)
                        .with_color(sub_text)
                        .finish(),
                )
                .with_margin_left(12.)
                .finish(),
            );
        }
        Some(row.finish())
    }
}

fn ward_remote_phase(tab: &RemoteTab, model: &TerminalModel) -> Phase {
    let block = model.block_list().active_block();
    tab.phase(block.id().as_str(), block.finished())
}
