//! Ward (OSS build) hooks of the terminal for agent sessions watched from another machine.
//! Kept out of `view.rs` so the upstream file only carries small calls.
//!
//! On the machine of a session, its tab takes the size of the viewer in Ward on another machine
//! for as long as the stream lasts (`crate::ward::agents::stream`). It goes through the same
//! path as Warp's viewer-driven sizing of shared sessions: the pane keeps its pixels, the grid
//! and the PTY get the viewer's columns and rows, and a later layout does not undo it.

#[cfg(not(target_arch = "wasm32"))]
use session_sharing_protocol::common::WindowSize;
use warpui::ViewContext;

use super::TerminalView;

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
}
