//! The card chip of a bound tab, `5/1287 In progress`, shared by the tab row and the CLI agent
//! footer.

use warp_core::ui::color::coloru_with_opacity;
use warpui::color::ColorU;
use warpui::elements::{Container, CornerRadius, Element, Radius, Text};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, EntityId};

use super::api::{CardStatus, ColumnKind, chip_text};
use super::bindings;
use super::model::model;
use crate::appearance::Appearance;

fn chip_color(status: Option<&CardStatus>, appearance: &Appearance) -> ColorU {
    let theme = appearance.theme();
    match status {
        Some(status) if status.archived => theme.sub_text_color(theme.background()).into_solid(),
        Some(status) => match status.column_kind {
            ColumnKind::Active | ColumnKind::Progress => theme.ansi_fg_cyan(),
            ColumnKind::Done | ColumnKind::DoneSprint => theme.ansi_fg_green(),
            ColumnKind::Sprint | ColumnKind::Backlog => theme.ansi_fg_magenta(),
        },
        None => theme.sub_text_color(theme.background()).into_solid(),
    }
}

/// The chip of the card bound to the terminal's pane, if any.
pub fn render_card_chip(
    terminal_view_id: EntityId,
    appearance: &Appearance,
    app: &AppContext,
) -> Option<Box<dyn Element>> {
    if !crate::features::is_fork_oss() {
        return None;
    }
    let card = bindings::card_of_view(terminal_view_id, app)?;
    let status = model(app).and_then(|model| model.card_status(card));
    let color = chip_color(status, appearance);
    let text = chip_text(card.board_id, card.task_id, status);
    Some(
        Container::new(
            Text::new_inline(text, appearance.ui_font_family(), 11.)
                .with_clip(ClipConfig::ellipsis())
                .with_color(color)
                .finish(),
        )
        .with_horizontal_padding(6.)
        .with_vertical_padding(1.)
        .with_background(coloru_with_opacity(color, 14))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(5.)))
        .finish(),
    )
}
