//! Ward (OSS build) chips of the CLI agent footer. They are fixed elements next to the
//! configurable toolbar items, so saved toolbar layouts stay untouched.

use warp_core::ui::color::coloru_with_opacity;
use warpui::elements::{
    Container, CornerRadius, CrossAxisAlignment, DispatchEventResult, Element, EventHandler, Flex,
    MainAxisSize, ParentElement, Radius, Text,
};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, EntityId, SingletonEntity, View, ViewContext};

use crate::appearance::Appearance;
use crate::util::bindings::keybinding_name_to_display_string;
use crate::ward::attention::{self, jump_order, next_other_target};
use crate::ward::chimney::model::ChimneyModel;
use crate::ward::labels::clip_chars;
use crate::workspace::WorkspaceAction;

const MAX_NEXT_TASK_CHARS: usize = 32;

/// Re-renders the footer when a Chimney board was fetched, for the card chip.
pub(super) fn subscribe_to_chimney<V: View>(ctx: &mut ViewContext<V>) {
    if !ctx.has_singleton_model::<ChimneyModel>() {
        return;
    }
    ctx.subscribe_to_model(&ChimneyModel::handle(ctx), |_, _, _, ctx| ctx.notify());
}

/// "Next: <task> Ctrl+Shift+J": the most urgent other agent waiting for the user. Clicking it
/// jumps there like the shortcut does.
pub(super) fn render_next_waiting_chip(
    terminal_view_id: EntityId,
    appearance: &Appearance,
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    if !crate::features::is_fork_oss() {
        return None;
    }
    let entries = attention::all_agent_entries(app);
    let order = jump_order(
        &entries
            .iter()
            .map(|entry| {
                (
                    entry.terminal_view.id(),
                    entry.snapshot.state,
                    entry.snapshot.in_state,
                )
            })
            .collect::<Vec<_>>(),
    );
    let target = next_other_target(&order, terminal_view_id)?;
    let entry = entries
        .iter()
        .find(|entry| entry.terminal_view.id() == target)?;
    let theme = appearance.theme();
    let color = entry.snapshot.state.color(theme);
    let font = appearance.ui_font_family();
    let mut content = Flex::row()
        .with_main_axis_size(MainAxisSize::Min)
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_spacing(6.)
        .with_child(
            Text::new_inline(
                format!(
                    "Next: {}",
                    clip_chars(&entry.labels.task, MAX_NEXT_TASK_CHARS)
                ),
                font,
                12.,
            )
            .with_clip(ClipConfig::ellipsis())
            .with_color(color)
            .finish(),
        );
    if let Some(shortcut) =
        keybinding_name_to_display_string("workspace:ward_jump_to_waiting_agent", app)
    {
        content.add_child(
            Text::new_inline(shortcut, font, 10.)
                .with_color(theme.sub_text_color(theme.background()).into_solid())
                .finish(),
        );
    }
    let chip = Container::new(content.finish())
        .with_horizontal_padding(8.)
        .with_vertical_padding(3.)
        .with_background(coloru_with_opacity(color, 13))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
        .finish();
    Some(
        EventHandler::new(chip)
            .on_left_mouse_down(|ctx, _, _| {
                ctx.dispatch_typed_action(WorkspaceAction::WardJumpToWaitingAgent);
                DispatchEventResult::StopPropagation
            })
            .finish(),
    )
}
