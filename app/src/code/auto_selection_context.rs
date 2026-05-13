use std::{collections::HashMap, ops::Range};

use warpui::{Entity, EntityId, ModelContext, SingletonEntity, WindowId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeSelectionContext {
    pub terminal_view_id: EntityId,
    pub relative_file_path: String,
    pub line_range: Range<usize>,
    pub selected_text: String,
}

#[derive(Default)]
pub struct AutoCodeSelectionContextModel {
    selections_by_window: HashMap<WindowId, HashMap<EntityId, CodeSelectionContext>>,
}

impl AutoCodeSelectionContextModel {
    pub fn set_selection(
        &mut self,
        window_id: WindowId,
        source_id: EntityId,
        selection: Option<CodeSelectionContext>,
        _ctx: &mut ModelContext<Self>,
    ) {
        let Some(selection) = selection else {
            self.remove_selection(window_id, source_id);
            return;
        };

        self.selections_by_window
            .entry(window_id)
            .or_default()
            .insert(source_id, selection);
    }

    pub fn remove_selection(&mut self, window_id: WindowId, source_id: EntityId) {
        if let Some(selections) = self.selections_by_window.get_mut(&window_id) {
            selections.remove(&source_id);
            if selections.is_empty() {
                self.selections_by_window.remove(&window_id);
            }
        }
    }

    pub fn unique_selection_for_window(&self, window_id: WindowId) -> Option<CodeSelectionContext> {
        let mut selections = self.selections_by_window.get(&window_id)?.values();
        let selection = selections.next()?.clone();
        selections.next().is_none().then_some(selection)
    }

    pub fn take_unique_selection_for_terminal_view(
        &mut self,
        window_id: WindowId,
        terminal_view_id: EntityId,
    ) -> Option<CodeSelectionContext> {
        let selections = self.selections_by_window.get(&window_id)?;
        let mut matching_selections = selections
            .iter()
            .filter(|(_, selection)| selection.terminal_view_id == terminal_view_id);
        let (source_id, selection) = matching_selections.next()?;
        if matching_selections.next().is_some() {
            return None;
        }

        let source_id = *source_id;
        let selection = selection.clone();
        self.remove_selection(window_id, source_id);

        Some(selection)
    }
}

impl Entity for AutoCodeSelectionContextModel {
    type Event = ();
}

impl SingletonEntity for AutoCodeSelectionContextModel {}

#[cfg(test)]
mod tests {
    use super::*;

    fn selection(path: &str) -> CodeSelectionContext {
        CodeSelectionContext {
            terminal_view_id: EntityId::new(),
            relative_file_path: path.to_string(),
            line_range: 1..2,
            selected_text: "let value = 1;".to_string(),
        }
    }

    fn selection_for_terminal(path: &str, terminal_view_id: EntityId) -> CodeSelectionContext {
        CodeSelectionContext {
            terminal_view_id,
            relative_file_path: path.to_string(),
            line_range: 1..2,
            selected_text: "let value = 1;".to_string(),
        }
    }

    #[test]
    fn unique_selection_returns_only_selection_for_window() {
        let window_id = WindowId::new();
        let source_id = EntityId::new();
        let terminal_view_id = EntityId::new();
        let mut model = AutoCodeSelectionContextModel::default();
        model
            .selections_by_window
            .entry(window_id)
            .or_default()
            .insert(
                source_id,
                selection_for_terminal("src/main.rs", terminal_view_id),
            );

        assert_eq!(
            model.unique_selection_for_window(window_id),
            Some(selection_for_terminal("src/main.rs", terminal_view_id))
        );
    }

    #[test]
    fn unique_selection_returns_none_for_ambiguous_window() {
        let window_id = WindowId::new();
        let mut model = AutoCodeSelectionContextModel::default();
        let selections = model.selections_by_window.entry(window_id).or_default();
        selections.insert(EntityId::new(), selection("src/one.rs"));
        selections.insert(EntityId::new(), selection("src/two.rs"));

        assert_eq!(model.unique_selection_for_window(window_id), None);
    }

    #[test]
    fn remove_selection_clears_empty_window_bucket() {
        let window_id = WindowId::new();
        let source_id = EntityId::new();
        let mut model = AutoCodeSelectionContextModel::default();
        model
            .selections_by_window
            .entry(window_id)
            .or_default()
            .insert(source_id, selection("src/main.rs"));

        model.remove_selection(window_id, source_id);

        assert_eq!(model.unique_selection_for_window(window_id), None);
        assert!(!model.selections_by_window.contains_key(&window_id));
    }

    #[test]
    fn take_unique_selection_for_terminal_view_scopes_and_consumes_selection() {
        let window_id = WindowId::new();
        let matching_terminal_view_id = EntityId::new();
        let other_terminal_view_id = EntityId::new();
        let mut model = AutoCodeSelectionContextModel::default();
        model
            .selections_by_window
            .entry(window_id)
            .or_default()
            .insert(
                EntityId::new(),
                selection_for_terminal("src/main.rs", matching_terminal_view_id),
            );
        model
            .selections_by_window
            .entry(window_id)
            .or_default()
            .insert(
                EntityId::new(),
                selection_for_terminal("src/other.rs", other_terminal_view_id),
            );

        assert_eq!(
            model.take_unique_selection_for_terminal_view(window_id, matching_terminal_view_id),
            Some(selection_for_terminal(
                "src/main.rs",
                matching_terminal_view_id
            ))
        );
        assert_eq!(
            model.take_unique_selection_for_terminal_view(window_id, matching_terminal_view_id),
            None
        );
        assert_eq!(
            model.take_unique_selection_for_terminal_view(window_id, other_terminal_view_id),
            Some(selection_for_terminal(
                "src/other.rs",
                other_terminal_view_id
            ))
        );
    }
}
