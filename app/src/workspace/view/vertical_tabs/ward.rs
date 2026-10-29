//! Ward (OSS build) additions to the vertical tabs panel: the agent state line in terminal rows.
//! Kept out of `vertical_tabs.rs` so the upstream file only carries small hooks.

use warp_core::ui::theme::Fill as WarpThemeFill;
use warpui::elements::{
    ConstrainedBox, Container, CrossAxisAlignment, Element, Flex, MainAxisSize, ParentElement,
    Shrinkable, Text,
};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, EntityId};

use crate::appearance::Appearance;
use crate::ward::agent_state::{AgentState, agent_snapshot, format_in_state};

const STATE_GLYPH_SIZE: f32 = 12.;

/// Row tint for agents that need the user (waiting or failed). `None` keeps the default row.
pub(super) fn row_highlight_color(
    terminal_view_id: EntityId,
    app: &AppContext,
) -> Option<WarpThemeFill> {
    let snapshot = agent_snapshot(terminal_view_id, app)?;
    snapshot
        .state
        .needs_attention()
        .then(|| snapshot.state.color(Appearance::as_ref(app).theme()).into())
}

/// The agent line under a terminal row's title: state glyph, detail (question, tool and
/// argument, error) and time in the current state.
pub(super) fn render_agent_status_line(
    terminal_view_id: EntityId,
    appearance: &Appearance,
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    let snapshot = agent_snapshot(terminal_view_id, app)?;
    let theme = appearance.theme();
    let state_color = snapshot.state.color(theme);
    let sub_text = theme.sub_text_color(theme.background());
    // Only states that need the user color their text; the rest stay quiet.
    let detail_color: WarpThemeFill = if snapshot.state.needs_attention() {
        state_color.into()
    } else if snapshot.state == AgentState::Idle {
        theme.hint_text_color(theme.background())
    } else {
        sub_text
    };
    let font_family = appearance.ui_font_family();

    let glyph = ConstrainedBox::new(
        snapshot
            .state
            .icon()
            .to_warpui_icon(state_color.into())
            .finish(),
    )
    .with_width(STATE_GLYPH_SIZE)
    .with_height(STATE_GLYPH_SIZE)
    .finish();
    let detail = snapshot
        .detail
        .clone()
        .unwrap_or_else(|| snapshot.state.label().to_owned());
    let detail = Text::new_inline(detail, font_family, 12.)
        .with_clip(ClipConfig::ellipsis())
        .with_color(detail_color.into())
        .finish();
    let elapsed = Text::new_inline(format_in_state(snapshot.in_state), font_family, 11.)
        .with_color(sub_text.into())
        .finish();

    Some(
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(4.)
            .with_child(glyph)
            .with_child(Shrinkable::new(1., detail).finish())
            .with_child(Container::new(elapsed).with_margin_left(4.).finish())
            .finish(),
    )
}
