//! Ward (OSS build) Chimney card picker: the cards of the configured board, work in progress
//! first. Opened only by Ward (tab menu "Bind to Chimney card…", start agent from card); Enter
//! hands the card to the workspace.

use ordered_float::OrderedFloat;
use warpui::elements::{ConstrainedBox, Container, Flex, ParentElement, Text};
use warpui::fonts::{Properties, Weight};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, Element, Entity, SingletonEntity};

use crate::appearance::Appearance;
use crate::search::QueryFilter;
use crate::search::command_palette::mixer::CommandPaletteItemAction;
use crate::search::command_palette::render_util::render_search_item_icon;
use crate::search::command_palette::separator_search_item::SeparatorSearchItem;
use crate::search::data_source::{Query, QueryResult};
use crate::search::item::{IconLocation, SearchItem as SearchItemTrait};
use crate::search::mixer::{DataSourceRunErrorWrapper, SyncDataSource};
use crate::search::result_renderer::ItemHighlightState;
use crate::ui_components::icons::Icon;
use crate::ward::chimney::api::{CardChoice, ColumnKind, picker_cards};
use crate::ward::chimney::{self, picker};

#[derive(Default)]
pub struct DataSource;

impl Entity for DataSource {
    type Event = ();
}

fn message(text: String) -> QueryResult<CommandPaletteItemAction> {
    SeparatorSearchItem::new(text).into()
}

impl SyncDataSource for DataSource {
    type Action = CommandPaletteItemAction;

    fn run_query(
        &self,
        query: &Query,
        app: &AppContext,
    ) -> Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper> {
        // Sync sources also run for unfiltered queries; cards only show in the picker mode.
        if !query.filters.contains(&QueryFilter::WardChimneyCards) {
            return Ok(Vec::new());
        }
        let board_id = chimney::board_id(app);
        let Some(model) = chimney::model::model(app) else {
            return Ok(vec![message("Chimney is not available".to_owned())]);
        };
        let Some(board) = model.board(board_id) else {
            let text = match model.error(board_id) {
                Some(err) => format!("Chimney board {board_id}: {}", err.message()),
                None => format!("Loading Chimney board {board_id}…"),
            };
            return Ok(vec![message(text)]);
        };
        let include_done = picker::pending().is_some_and(|purpose| purpose.include_done());
        let cards = picker::filter_cards(picker_cards(board, include_done), &query.text);
        if cards.is_empty() {
            return Ok(vec![message(format!(
                "No matching cards on \u{201c}{}\u{201d}",
                board.title
            ))]);
        }
        Ok(cards
            .into_iter()
            .enumerate()
            .map(|(rank, card)| QueryResult::from(SearchItem { card, rank }))
            .collect())
    }
}

pub struct SearchItem {
    card: CardChoice,
    rank: usize,
}

impl SearchItemTrait for SearchItem {
    type Action = CommandPaletteItemAction;

    fn is_multiline(&self) -> bool {
        true
    }

    fn render_icon(
        &self,
        highlight_state: ItemHighlightState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let color = match self.card.column_kind {
            ColumnKind::Active | ColumnKind::Progress => theme.ansi_fg_cyan(),
            ColumnKind::Done | ColumnKind::DoneSprint => theme.ansi_fg_green(),
            ColumnKind::Sprint | ColumnKind::Backlog => theme.ansi_fg_magenta(),
        };
        render_search_item_icon(appearance, Icon::Workflow, color, highlight_state)
    }

    fn icon_location(&self, appearance: &Appearance) -> IconLocation {
        let margin_top = (appearance.line_height_ratio() * appearance.monospace_font_size())
            - appearance.monospace_font_size();
        IconLocation::Top { margin_top }
    }

    fn render_item(
        &self,
        highlight_state: ItemHighlightState,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let font = appearance.ui_font_family();
        let size = appearance.monospace_font_size();
        let sub_text = highlight_state.sub_text_fill(appearance).into_solid();

        let title = Text::new_inline(self.card.title.clone(), font, size)
            .with_clip(ClipConfig::ellipsis())
            .with_color(highlight_state.main_text_fill(appearance).into_solid())
            .with_style(Properties::default().weight(Weight::Bold))
            .finish();
        let mut detail = format!(
            "{}/{} \u{b7} {}",
            self.card.board_id, self.card.task_id, self.card.column
        );
        if !self.card.labels.is_empty() {
            detail.push_str(" \u{b7} ");
            detail.push_str(&self.card.labels.join(", "));
        }
        let contents = Flex::column()
            .with_child(title)
            .with_child(
                Container::new(
                    Text::new_inline(detail, font, size - 2.)
                        .with_clip(ClipConfig::ellipsis())
                        .with_color(sub_text)
                        .finish(),
                )
                .with_padding_top(4.)
                .finish(),
            )
            .finish();
        ConstrainedBox::new(contents).with_height(50.).finish()
    }

    fn score(&self) -> OrderedFloat<f64> {
        // The data source already ordered the cards.
        OrderedFloat::from(1000.0 - self.rank as f64)
    }

    fn accept_result(&self) -> Self::Action {
        CommandPaletteItemAction::WardPickChimneyCard {
            board_id: self.card.board_id,
            task_id: self.card.task_id,
        }
    }

    fn execute_result(&self) -> Self::Action {
        self.accept_result()
    }

    fn accessibility_label(&self) -> String {
        format!("Chimney card: {}. {}.", self.card.title, self.card.column)
    }

    fn accessibility_help_message(&self) -> Option<String> {
        Some(format!("Press enter to pick card: {}.", self.card.title))
    }
}
