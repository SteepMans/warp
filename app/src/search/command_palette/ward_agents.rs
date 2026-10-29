//! Ward (OSS build) `@` palette mode: CLI agents of all windows, most urgent first, with the
//! question or tool line under the task name. Enter focuses the agent's tab.

use ordered_float::OrderedFloat;
use warpui::elements::{ConstrainedBox, Container, Flex, ParentElement, Text};
use warpui::fonts::{Properties, Weight};
use warpui::text_layout::ClipConfig;
use warpui::{AppContext, Element, Entity, SingletonEntity};

use crate::appearance::Appearance;
use crate::search::command_palette::mixer::CommandPaletteItemAction;
use crate::search::command_palette::render_util::render_search_item_icon;
use crate::search::QueryFilter;
use crate::search::data_source::{Query, QueryResult};
use crate::search::item::{IconLocation, SearchItem as SearchItemTrait};
use crate::search::mixer::{DataSourceRunErrorWrapper, SyncDataSource};
use crate::search::result_renderer::ItemHighlightState;
use crate::ward::agent_state::format_in_state;
use crate::ward::palette::{PaletteAgent, filter_and_sort, palette_agents, section_label};

#[derive(Default)]
pub struct DataSource;

impl Entity for DataSource {
    type Event = ();
}

impl SyncDataSource for DataSource {
    type Action = CommandPaletteItemAction;

    fn run_query(
        &self,
        query: &Query,
        app: &AppContext,
    ) -> Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper> {
        // Sync sources also run for unfiltered queries; agents only show in the `@` mode.
        if !query.filters.contains(&QueryFilter::WardAgents) {
            return Ok(Vec::new());
        }
        let agents = filter_and_sort(palette_agents(app), &query.text);
        let mut previous_section = None;
        Ok(agents
            .into_iter()
            .enumerate()
            .map(|(rank, agent)| {
                let section = section_label(agent.state);
                let starts_section = previous_section != Some(section);
                previous_section = Some(section);
                QueryResult::from(SearchItem {
                    agent,
                    rank,
                    section: starts_section.then_some(section),
                })
            })
            .collect())
    }
}

pub struct SearchItem {
    agent: PaletteAgent,
    rank: usize,
    /// Set on the first agent of an urgency group.
    section: Option<&'static str>,
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
        render_search_item_icon(
            appearance,
            self.agent.state.icon(),
            self.agent.state.color(appearance.theme()),
            highlight_state,
        )
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

        let mut title = Flex::row().with_spacing(8.).with_child(
            Text::new_inline(self.agent.task.clone(), font, size)
                .with_clip(ClipConfig::ellipsis())
                .with_color(highlight_state.main_text_fill(appearance).into_solid())
                .with_style(Properties::default().weight(Weight::Bold))
                .finish(),
        );
        if let Some(project) = &self.agent.project {
            title.add_child(
                Text::new_inline(project.clone(), font, size - 2.)
                    .with_color(sub_text)
                    .finish(),
            );
        }
        title.add_child(
            Text::new_inline(format_in_state(self.agent.in_state), font, size - 2.)
                .with_color(sub_text)
                .finish(),
        );
        if let Some(section) = self.section {
            title.add_child(
                Text::new_inline(section, font, size - 3.)
                    .with_color(sub_text)
                    .finish(),
            );
        }

        let detail_color = if self.agent.state.needs_attention() {
            self.agent.state.color(appearance.theme())
        } else {
            sub_text
        };
        let detail = self
            .agent
            .detail
            .clone()
            .unwrap_or_else(|| self.agent.state.label().to_owned());
        let contents = Flex::column()
            .with_child(title.finish())
            .with_child(
                Container::new(
                    Text::new_inline(detail, font, size - 2.)
                        .with_clip(ClipConfig::ellipsis())
                        .with_color(detail_color)
                        .finish(),
                )
                .with_padding_top(4.)
                .finish(),
            )
            .finish();
        ConstrainedBox::new(contents).with_height(50.).finish()
    }

    fn score(&self) -> OrderedFloat<f64> {
        // The data source already ordered the agents by urgency.
        OrderedFloat::from(1000.0 - self.rank as f64)
    }

    fn accept_result(&self) -> Self::Action {
        CommandPaletteItemAction::WardFocusAgent {
            terminal_view_id: self.agent.terminal_view_id,
        }
    }

    fn execute_result(&self) -> Self::Action {
        self.accept_result()
    }

    fn accessibility_label(&self) -> String {
        format!("Agent: {}. {}.", self.agent.task, self.agent.state.label())
    }

    fn accessibility_help_message(&self) -> Option<String> {
        Some(format!("Press enter to open agent: {}.", self.agent.task))
    }
}
