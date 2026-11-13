//! The activity mark of a CLI agent on its tab: animated dots while the agent works, the state
//! glyph while it waits for the user or has failed. A finished run keeps the unread dot of the
//! notifications model, an idle agent shows nothing.
//!
//! The state comes from [`agent_snapshot`], the same source as the agent line of an expanded row,
//! so a compact or pinned row and the horizontal tab bar tell the same story.

use std::time::Duration;

use pathfinder_geometry::vector::{Vector2F, vec2f};
use warp_core::ui::color::coloru_with_opacity;
use warp_core::ui::theme::WarpTheme;
use warpui::color::ColorU;
use warpui::elements::animation::AnimationClock;
use warpui::elements::{
    ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Flex, MainAxisSize, ParentElement,
    Point, Radius, Rect,
};
use warpui::event::DispatchedEvent;
use warpui::{
    AfterLayoutContext, AppContext, Element, EntityId, EventContext, LayoutContext, PaintContext,
    SingletonEntity, SizeConstraint,
};

use super::agent_state::{AgentState, agent_snapshot};
use crate::appearance::Appearance;

/// Dots of the working animation.
pub const WORKING_DOTS: usize = 3;

/// How long each dot stays lit. Every frame repaints the window, so the animation is slow on
/// purpose: about two and a half frames a second while an agent works.
pub const WORKING_FRAME: Duration = Duration::from_millis(400);

/// Opacity of the dots that are not lit, in percent.
const DIM_DOT_OPACITY: u8 = 35;

/// What the tab of an agent shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabActivity {
    /// The agent is thinking or running a tool: animated dots in the state color.
    Working,
    /// The agent waits for the user (question, permission) or the turn failed: the state glyph.
    NeedsUser,
}

impl TabActivity {
    /// `None` for a finished, seen or idle agent: a finished run that was not seen yet keeps the
    /// unread dot of the notifications model, the rest stays quiet.
    pub fn of(state: AgentState) -> Option<Self> {
        if state.is_running() {
            Some(Self::Working)
        } else if state.needs_attention() {
            Some(Self::NeedsUser)
        } else {
            None
        }
    }
}

/// The lit dot `elapsed` into the animation and how long it stays lit.
pub fn working_frame_at(elapsed: Duration) -> (usize, Duration) {
    let frame = WORKING_FRAME.as_nanos();
    let elapsed = elapsed.as_nanos();
    let index = (elapsed / frame) % WORKING_DOTS as u128;
    let left = frame - elapsed % frame;
    (index as usize, Duration::from_nanos(left as u64))
}

/// The activity mark of the agent in `terminal_view_id`, `glyph_size` pixels high. `None`
/// outside the OSS build, for plain terminals, agents without plugin status and agents that
/// need nothing.
pub fn render_tab_activity(
    terminal_view_id: EntityId,
    glyph_size: f32,
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    let snapshot = agent_snapshot(terminal_view_id, app)?;
    render_activity(
        snapshot.state,
        snapshot.in_state,
        Appearance::as_ref(app).theme(),
        glyph_size,
    )
}

/// The activity mark of an agent in `state` for `in_state`; `None` when the state needs no mark.
pub fn render_activity(
    state: AgentState,
    in_state: Duration,
    theme: &WarpTheme,
    glyph_size: f32,
) -> Option<Box<dyn Element>> {
    let color = state.color(theme);
    Some(match TabActivity::of(state)? {
        TabActivity::Working => render_working_dots(color, glyph_size, in_state),
        TabActivity::NeedsUser => {
            ConstrainedBox::new(state.icon().to_warpui_icon(color.into()).finish())
                .with_width(glyph_size)
                .with_height(glyph_size)
                .finish()
        }
    })
}

/// Three dots lit one after another. `in_state` keeps the phase across re-renders of the row.
fn render_working_dots(color: ColorU, glyph_size: f32, in_state: Duration) -> Box<dyn Element> {
    let dot = (glyph_size / 3.).round().max(3.);
    let frames = (0..WORKING_DOTS)
        .map(|lit| {
            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Min)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(dot / 2.);
            for index in 0..WORKING_DOTS {
                let dot_color = if index == lit {
                    color
                } else {
                    coloru_with_opacity(color, DIM_DOT_OPACITY)
                };
                row.add_child(
                    ConstrainedBox::new(
                        Rect::new()
                            .with_background_color(dot_color)
                            .with_corner_radius(CornerRadius::with_all(Radius::Percentage(50.)))
                            .finish(),
                    )
                    .with_width(dot)
                    .with_height(dot)
                    .finish(),
                );
            }
            Container::new(row.finish())
                .with_vertical_padding(((glyph_size - dot) / 2.).max(0.))
                .finish()
        })
        .collect();
    WorkingDots {
        frames,
        clock: AnimationClock::starting_at(in_state),
        size: None,
        origin: None,
    }
    .finish()
}

/// Paints one of its frames, picked by the clock at paint time, and asks for a repaint when the
/// next frame is due. A repaint does not render the view again, so the frame cannot be chosen
/// at render time.
struct WorkingDots {
    frames: Vec<Box<dyn Element>>,
    clock: AnimationClock,
    size: Option<Vector2F>,
    origin: Option<Point>,
}

impl Element for WorkingDots {
    fn layout(
        &mut self,
        constraint: SizeConstraint,
        ctx: &mut LayoutContext,
        app: &AppContext,
    ) -> Vector2F {
        let size = self
            .frames
            .iter_mut()
            .map(|frame| frame.layout(constraint, ctx, app))
            .fold(vec2f(0., 0.), |a, b| a.max(b));
        self.size = Some(size);
        size
    }

    fn after_layout(&mut self, ctx: &mut AfterLayoutContext, app: &AppContext) {
        for frame in &mut self.frames {
            frame.after_layout(ctx, app);
        }
    }

    fn paint(&mut self, origin: Vector2F, ctx: &mut PaintContext, app: &AppContext) {
        self.origin = Some(Point::from_vec2f(origin, ctx.scene.z_index()));
        let (index, left) = working_frame_at(self.clock.elapsed());
        if let Some(frame) = self.frames.get_mut(index) {
            frame.paint(origin, ctx, app);
        }
        ctx.repaint_after(left);
    }

    fn size(&self) -> Option<Vector2F> {
        self.size
    }

    fn origin(&self) -> Option<Point> {
        self.origin
    }

    fn dispatch_event(
        &mut self,
        _: &DispatchedEvent,
        _: &mut EventContext,
        _: &AppContext,
    ) -> bool {
        false
    }
}

#[cfg(test)]
#[path = "tab_activity_tests.rs"]
mod tests;
