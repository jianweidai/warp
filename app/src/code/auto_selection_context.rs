use std::{collections::HashMap, ops::Range};

use warpui::{Entity, EntityId, ModelContext, SingletonEntity, WindowId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeSelectionContext {
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
            relative_file_path: path.to_string(),
            line_range: 1..2,
            selected_text: "let value = 1;".to_string(),
        }
    }

    #[test]
    fn unique_selection_returns_only_selection_for_window() {
        let window_id = WindowId::new();
        let source_id = EntityId::new();
        let mut model = AutoCodeSelectionContextModel::default();
        model
            .selections_by_window
            .entry(window_id)
            .or_default()
            .insert(source_id, selection("src/main.rs"));

        assert_eq!(
            model.unique_selection_for_window(window_id),
            Some(selection("src/main.rs"))
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
}
