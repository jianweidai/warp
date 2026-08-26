use std::path::PathBuf;

use crate::appearance::Appearance;
use crate::code::editor::{add_color, remove_color};
use crate::code_review::diff_state::{DiffHunk, DiffLineType, DiffStateModel};
use crate::util::git::{
    get_commit_file_diff, get_working_tree_file_diff, GitHistoryFileDiff, GitWorkingTreeArea,
    GitWorkingTreeChange,
};
use warpui::elements::{
    Border, ClippedScrollStateHandle, ClippedScrollable, Container, CornerRadius,
    CrossAxisAlignment, Element, Fill, Flex, MainAxisSize, ParentElement, ScrollbarWidth,
    Shrinkable, Text,
};
use warpui::r#async::SpawnedFutureHandle;
use warpui::{AppContext, Entity, SingletonEntity, View, ViewContext};

#[derive(Clone, Debug)]
pub enum GitHistoryDiffEvent {
    Updated,
}

#[derive(Clone, Debug)]
struct GitHistoryDiffSelection {
    repo_path: PathBuf,
    file_path: String,
    source: GitHistoryDiffSource,
}

#[derive(Clone, Debug)]
enum GitHistoryDiffSource {
    Commit {
        commit_hash: String,
        commit_subject: String,
    },
    WorkingTree {
        area: GitWorkingTreeArea,
        change: GitWorkingTreeChange,
    },
}

enum GitHistoryDiffState {
    Empty,
    Loading(GitHistoryDiffSelection),
    Loaded {
        selection: GitHistoryDiffSelection,
        hunks: Vec<DiffHunk>,
        is_binary: bool,
        is_too_large: bool,
        has_hidden_bidi_chars: bool,
    },
    Error {
        selection: GitHistoryDiffSelection,
        message: String,
    },
}

/// 右侧只读历史 Diff 面板。
pub struct GitHistoryDiffView {
    state: GitHistoryDiffState,
    request_generation: u64,
    request_abort_handle: Option<SpawnedFutureHandle>,
    scroll_state: ClippedScrollStateHandle,
}

impl Entity for GitHistoryDiffView {
    type Event = GitHistoryDiffEvent;
}

impl GitHistoryDiffView {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self {
            state: GitHistoryDiffState::Empty,
            request_generation: 0,
            request_abort_handle: None,
            scroll_state: ClippedScrollStateHandle::default(),
        }
    }

    pub fn open(
        &mut self,
        repo_path: PathBuf,
        commit_hash: String,
        commit_subject: String,
        file_path: String,
        ctx: &mut ViewContext<Self>,
    ) {
        self.open_selection(
            GitHistoryDiffSelection {
                repo_path,
                file_path,
                source: GitHistoryDiffSource::Commit {
                    commit_hash,
                    commit_subject,
                },
            },
            ctx,
        );
    }

    pub fn open_working_tree(
        &mut self,
        repo_path: PathBuf,
        area: GitWorkingTreeArea,
        change: GitWorkingTreeChange,
        ctx: &mut ViewContext<Self>,
    ) {
        self.open_selection(
            GitHistoryDiffSelection {
                repo_path,
                file_path: change.path.clone(),
                source: GitHistoryDiffSource::WorkingTree { area, change },
            },
            ctx,
        );
    }

    fn open_selection(&mut self, selection: GitHistoryDiffSelection, ctx: &mut ViewContext<Self>) {
        self.abort_request();
        self.request_generation = self.request_generation.wrapping_add(1);
        let request_generation = self.request_generation;
        let request_file_path = selection.file_path.clone();
        let completion_selection = selection.clone();
        let repo_path = selection.repo_path.clone();
        let source = selection.source.clone();

        self.state = GitHistoryDiffState::Loading(selection);
        ctx.emit(GitHistoryDiffEvent::Updated);
        ctx.notify();

        self.request_abort_handle = Some(ctx.spawn(
            async move {
                let diff = match source {
                    GitHistoryDiffSource::Commit { commit_hash, .. } => {
                        get_commit_file_diff(&repo_path, &commit_hash, &request_file_path).await?
                    }
                    GitHistoryDiffSource::WorkingTree { area, change } => {
                        get_working_tree_file_diff(&repo_path, area, &change).await?
                    }
                };
                let has_hidden_bidi_chars =
                    DiffStateModel::check_for_hidden_bidi_chars(&diff.patch);
                let hunks = if diff.is_binary || diff.is_too_large {
                    Vec::new()
                } else {
                    DiffStateModel::parse_diff_hunks(&diff.patch)?
                };
                Ok::<(GitHistoryFileDiff, Vec<DiffHunk>, bool), anyhow::Error>((
                    diff,
                    hunks,
                    has_hidden_bidi_chars,
                ))
            },
            move |view, result, ctx| {
                if view.request_generation != request_generation {
                    return;
                }

                view.request_abort_handle = None;
                view.state = match result {
                    Ok((diff, hunks, has_hidden_bidi_chars)) => GitHistoryDiffState::Loaded {
                        selection: completion_selection.clone(),
                        hunks,
                        is_binary: diff.is_binary,
                        is_too_large: diff.is_too_large,
                        has_hidden_bidi_chars,
                    },
                    Err(error) => GitHistoryDiffState::Error {
                        selection: completion_selection,
                        message: error.to_string(),
                    },
                };
                ctx.emit(GitHistoryDiffEvent::Updated);
                ctx.notify();
            },
        ));
    }

    pub fn clear(&mut self, ctx: &mut ViewContext<Self>) {
        self.abort_request();
        self.request_generation = self.request_generation.wrapping_add(1);
        self.state = GitHistoryDiffState::Empty;
        ctx.emit(GitHistoryDiffEvent::Updated);
        ctx.notify();
    }

    fn abort_request(&mut self) {
        if let Some(handle) = self.request_abort_handle.take() {
            handle.abort();
        }
    }

    fn selection(&self) -> Option<&GitHistoryDiffSelection> {
        match &self.state {
            GitHistoryDiffState::Empty => None,
            GitHistoryDiffState::Loading(selection)
            | GitHistoryDiffState::Loaded { selection, .. }
            | GitHistoryDiffState::Error { selection, .. } => Some(selection),
        }
    }

    fn render_header(
        &self,
        selection: &GitHistoryDiffSelection,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let (title, source_label) = match &selection.source {
            GitHistoryDiffSource::Commit {
                commit_hash,
                commit_subject,
            } => {
                let short_hash = commit_hash.chars().take(8).collect::<String>();
                (
                    crate::t!("git-history-diff-title"),
                    format!("{short_hash} · {commit_subject}"),
                )
            }
            GitHistoryDiffSource::WorkingTree { area, .. } => {
                let area_label = match area {
                    GitWorkingTreeArea::Staged => crate::t!("git-working-tree-staged"),
                    GitWorkingTreeArea::Unstaged => crate::t!("git-working-tree-unstaged"),
                };
                (crate::t!("git-working-tree-diff-title"), area_label)
            }
        };
        let title = Text::new_inline(title, appearance.ui_font_family(), 14.)
            .with_color(theme.main_text_color(theme.background()).into())
            .finish();
        let source = Text::new_inline(source_label, appearance.ui_font_family(), 11.)
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish();
        let path = Text::new(
            selection.file_path.clone(),
            appearance.monospace_font_family(),
            11.,
        )
        .with_color(theme.accent().into())
        .soft_wrap(false)
        .finish();

        Container::new(
            Flex::column()
                .with_main_axis_size(MainAxisSize::Min)
                .with_child(title)
                .with_child(source)
                .with_child(path)
                .finish(),
        )
        .with_horizontal_padding(12.)
        .with_vertical_padding(10.)
        .with_border(Border::bottom(1.).with_border_fill(theme.surface_3()))
        .finish()
    }

    fn render_message(&self, message: String, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        Container::new(
            Text::new_inline(message, appearance.ui_font_family(), 12.)
                .with_color(
                    appearance
                        .theme()
                        .sub_text_color(appearance.theme().background())
                        .into(),
                )
                .finish(),
        )
        .with_horizontal_padding(12.)
        .with_vertical_padding(20.)
        .finish()
    }

    fn render_hunks(&self, hunks: &[DiffHunk], app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let mut rows = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        for hunk in hunks {
            let old_range = format_hunk_range(hunk.old_start_line, hunk.old_line_count);
            let new_range = format_hunk_range(hunk.new_start_line, hunk.new_line_count);
            let hunk_header = Text::new(
                format!("@@ -{old_range} +{new_range} @@"),
                appearance.monospace_font_family(),
                appearance.monospace_font_size(),
            )
            .with_color(theme.accent().into())
            .soft_wrap(false)
            .finish();
            rows.add_child(
                Container::new(hunk_header)
                    .with_background(theme.surface_2())
                    .with_horizontal_padding(12.)
                    .with_vertical_padding(4.)
                    .finish(),
            );

            for line in &hunk.lines {
                rows.add_child(self.render_diff_line(line, app));
            }
        }

        let scrollable = ClippedScrollable::vertical(
            self.scroll_state.clone(),
            Container::new(rows.finish()).finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();

        Container::new(scrollable)
            .with_background(theme.background())
            .with_corner_radius(CornerRadius::with_all(warpui::elements::Radius::Pixels(4.)))
            .finish()
    }

    fn render_diff_line(
        &self,
        line: &crate::code_review::diff_state::DiffLine,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let old_line_number = line
            .old_line_number
            .map_or_else(String::new, |number| number.to_string());
        let new_line_number = line
            .new_line_number
            .map_or_else(String::new, |number| number.to_string());
        let (prefix, color) = match &line.line_type {
            DiffLineType::Context => (" ", theme.main_text_color(theme.background()).into()),
            DiffLineType::Add => ("+", add_color(appearance)),
            DiffLineType::Delete => ("-", remove_color(appearance)),
            DiffLineType::HunkHeader => ("@", theme.accent().into()),
        };
        let text = &line.text;
        let line_text = format!("{old_line_number:>5} {new_line_number:>5} {prefix} {text}");
        let line_element = Text::new(
            line_text,
            appearance.monospace_font_family(),
            appearance.monospace_font_size(),
        )
        .with_color(color)
        .soft_wrap(false)
        .finish();

        Container::new(line_element)
            .with_horizontal_padding(12.)
            .with_vertical_padding(1.)
            .finish()
    }
}

impl Drop for GitHistoryDiffView {
    fn drop(&mut self) {
        self.abort_request();
    }
}

impl View for GitHistoryDiffView {
    fn ui_name() -> &'static str {
        "GitHistoryDiffView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let mut content = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch);

        if let Some(selection) = self.selection() {
            content.add_child(self.render_header(selection, app));
        }

        let body: Box<dyn Element> = match &self.state {
            GitHistoryDiffState::Empty => {
                self.render_message(crate::t!("git-history-diff-empty-selection"), app)
            }
            GitHistoryDiffState::Loading(selection) => self.render_message(
                format!(
                    "{}: {}",
                    crate::t!("git-history-diff-loading"),
                    selection.file_path
                ),
                app,
            ),
            GitHistoryDiffState::Loaded {
                hunks,
                is_binary,
                is_too_large,
                ..
            } => {
                if *is_binary {
                    self.render_message(crate::t!("git-history-diff-binary"), app)
                } else if *is_too_large {
                    self.render_message(crate::t!("git-history-diff-too-large"), app)
                } else if hunks.is_empty() {
                    self.render_message(crate::t!("git-history-diff-no-text-changes"), app)
                } else {
                    self.render_hunks(hunks, app)
                }
            }
            GitHistoryDiffState::Error { message, .. } => self.render_message(
                format!("{}: {message}", crate::t!("git-history-diff-load-error")),
                app,
            ),
        };
        if matches!(
            &self.state,
            GitHistoryDiffState::Loaded {
                has_hidden_bidi_chars: true,
                ..
            }
        ) {
            content.add_child(self.render_message(crate::t!("git-history-diff-hidden-bidi"), app));
        }
        content.add_child(Shrinkable::new(1., body).finish());

        Container::new(content.finish()).finish()
    }
}

fn format_hunk_range(start: usize, count: usize) -> String {
    if count == 1 {
        start.to_string()
    } else {
        format!("{start},{count}")
    }
}
