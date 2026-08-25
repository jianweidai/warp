use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

#[cfg(feature = "local_fs")]
use async_channel::Sender;

#[cfg(feature = "local_fs")]
use repo_metadata::{
    repositories::DetectedRepositories,
    repository::{Repository, RepositorySubscriber, SubscriberId},
    RepositoryUpdate,
};

use crate::appearance::Appearance;
use crate::pane_group::WorkingDirectoriesModel;
use crate::util::git::{get_commit_files, get_commit_history, FileChangeEntry, GitHistoryCommit};
use crate::view_components::action_button::{ActionButton, ButtonSize, NakedTheme, SecondaryTheme};
use warp_core::ui::Icon;
use warpui::elements::{
    Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, Container, CornerRadius,
    CrossAxisAlignment, Element, Flex, Hoverable, MainAxisAlignment, MainAxisSize,
    MouseStateHandle, ParentElement, ScrollbarWidth, Shrinkable,
};
use warpui::platform::Cursor;
use warpui::{
    AppContext, Entity, ModelContext, ModelHandle, TypedActionView, View, ViewContext, ViewHandle,
};

const PAGE_SIZE: usize = 50;

/// Git 历史侧栏支持的交互动作。
#[derive(Clone, Debug)]
pub enum GitHistoryAction {
    Refresh,
    LoadMore,
    ToggleCommit { hash: String },
}

/// Git 历史模型发生变化时通知视图重新渲染。
#[derive(Clone, Debug)]
pub enum GitHistoryEvent {
    Updated,
    OpenFileDiff {
        repo_path: PathBuf,
        commit_hash: String,
        commit_subject: String,
        file_path: String,
    },
}

/// 当前仓库的只读 Git 提交历史缓存。
pub struct GitHistoryModel {
    repo_path: PathBuf,
    commits: Vec<GitHistoryCommit>,
    has_more: bool,
    loading: bool,
    error: Option<String>,
    commit_files: HashMap<String, Vec<FileChangeEntry>>,
    loading_files: HashSet<String>,
    file_errors: HashMap<String, String>,
    request_generation: u64,
    request_abort_handle: Option<warpui::r#async::SpawnedFutureHandle>,
    #[cfg(feature = "local_fs")]
    repository: Option<ModelHandle<Repository>>,
    #[cfg(feature = "local_fs")]
    subscriber_id: Option<SubscriberId>,
}

impl Entity for GitHistoryModel {
    type Event = GitHistoryEvent;
}

impl GitHistoryModel {
    pub fn new(repo_path: PathBuf, ctx: &mut ModelContext<Self>) -> Self {
        let mut model = Self {
            repo_path,
            commits: Vec::new(),
            has_more: false,
            loading: false,
            error: None,
            commit_files: HashMap::new(),
            loading_files: HashSet::new(),
            file_errors: HashMap::new(),
            request_generation: 0,
            request_abort_handle: None,
            #[cfg(feature = "local_fs")]
            repository: None,
            #[cfg(feature = "local_fs")]
            subscriber_id: None,
        };

        #[cfg(feature = "local_fs")]
        if let Some(repository) =
            DetectedRepositories::as_ref(ctx).get_watched_repo_for_path(&model.repo_path, ctx)
        {
            model.start_watching(repository, ctx);
        }

        model.refresh(ctx);
        model
    }

    pub fn commits(&self) -> &[GitHistoryCommit] {
        &self.commits
    }

    pub fn has_more(&self) -> bool {
        self.has_more
    }

    pub fn is_loading(&self) -> bool {
        self.loading
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn commit_files(&self, hash: &str) -> Option<&Vec<FileChangeEntry>> {
        self.commit_files.get(hash)
    }

    pub fn is_loading_files(&self, hash: &str) -> bool {
        self.loading_files.contains(hash)
    }

    pub fn commit_file_error(&self, hash: &str) -> Option<&str> {
        self.file_errors.get(hash).map(String::as_str)
    }

    pub fn refresh(&mut self, ctx: &mut ModelContext<Self>) {
        self.abort_request();
        self.request_generation = self.request_generation.wrapping_add(1);
        let request_generation = self.request_generation;
        let repo_path = self.repo_path.clone();

        self.loading = true;
        self.error = None;
        ctx.emit(GitHistoryEvent::Updated);

        self.request_abort_handle = Some(ctx.spawn(
            async move { get_commit_history(&repo_path, 0, PAGE_SIZE).await },
            move |model, result, ctx| {
                if model.request_generation != request_generation {
                    return;
                }
                model.request_abort_handle = None;
                model.loading = false;
                match result {
                    Ok(page) => {
                        model.commits = page.commits;
                        model.has_more = page.has_more;
                        model.commit_files.clear();
                        model.file_errors.clear();
                    }
                    Err(error) => {
                        model.commits.clear();
                        model.has_more = false;
                        model.error = Some(error.to_string());
                    }
                }
                ctx.emit(GitHistoryEvent::Updated);
            },
        ));
    }

    pub fn load_more(&mut self, ctx: &mut ModelContext<Self>) {
        if self.loading || !self.has_more {
            return;
        }

        self.abort_request();
        self.request_generation = self.request_generation.wrapping_add(1);
        let request_generation = self.request_generation;
        let skip = self.commits.len();
        let repo_path = self.repo_path.clone();

        self.loading = true;
        self.error = None;
        ctx.emit(GitHistoryEvent::Updated);

        self.request_abort_handle = Some(ctx.spawn(
            async move { get_commit_history(&repo_path, skip, PAGE_SIZE).await },
            move |model, result, ctx| {
                if model.request_generation != request_generation {
                    return;
                }
                model.request_abort_handle = None;
                model.loading = false;
                match result {
                    Ok(page) => {
                        model.commits.extend(page.commits);
                        model.has_more = page.has_more;
                    }
                    Err(error) => {
                        model.error = Some(error.to_string());
                    }
                }
                ctx.emit(GitHistoryEvent::Updated);
            },
        ));
    }

    pub fn load_commit_files(&mut self, hash: String, ctx: &mut ModelContext<Self>) {
        if self.commit_files.contains_key(&hash) || !self.loading_files.insert(hash.clone()) {
            return;
        }

        self.file_errors.remove(&hash);
        let repo_path = self.repo_path.clone();
        let request_hash = hash.clone();
        ctx.emit(GitHistoryEvent::Updated);
        ctx.spawn(
            async move { get_commit_files(&repo_path, &request_hash).await },
            move |model, result, ctx| {
                model.loading_files.remove(&hash);
                match result {
                    Ok(files) => {
                        model.file_errors.remove(&hash);
                        model.commit_files.insert(hash, files);
                    }
                    Err(error) => {
                        model.file_errors.insert(hash, error.to_string());
                    }
                }
                ctx.emit(GitHistoryEvent::Updated);
            },
        );
    }

    fn abort_request(&mut self) {
        if let Some(handle) = self.request_abort_handle.take() {
            handle.abort();
        }
    }

    #[cfg(feature = "local_fs")]
    fn start_watching(
        &mut self,
        repository: ModelHandle<Repository>,
        ctx: &mut ModelContext<Self>,
    ) {
        let (repository_update_tx, repository_update_rx) = async_channel::unbounded();
        let start = repository.update(ctx, |repository, ctx| {
            repository.start_watching(
                Box::new(GitHistoryRepositorySubscriber {
                    repository_update_tx,
                }),
                ctx,
            )
        });

        self.repository = Some(repository);
        self.subscriber_id = Some(start.subscriber_id);
        ctx.spawn(start.registration_future, |model, result, ctx| {
            if let Err(error) = result {
                log::warn!("GitHistoryModel watcher registration failed: {error}");
                model.stop_active_watcher(ctx);
            }
        });

        ctx.spawn_stream_local(
            repository_update_rx,
            |model, update: RepositoryUpdate, ctx| {
                if update.commit_updated {
                    model.refresh(ctx);
                }
            },
            |_, _| {},
        );
    }

    #[cfg(feature = "local_fs")]
    pub fn stop_active_watcher(&mut self, ctx: &mut ModelContext<Self>) {
        if let Some(repository) = self.repository.as_ref() {
            if let Some(subscriber_id) = self.subscriber_id.take() {
                repository.update(ctx, |repository, ctx| {
                    repository.stop_watching(subscriber_id, ctx);
                });
            }
        }
    }
}

impl Drop for GitHistoryModel {
    fn drop(&mut self) {
        self.abort_request();
    }
}

#[cfg(feature = "local_fs")]
struct GitHistoryRepositorySubscriber {
    repository_update_tx: Sender<RepositoryUpdate>,
}

#[cfg(feature = "local_fs")]
impl RepositorySubscriber for GitHistoryRepositorySubscriber {
    fn on_scan(
        &mut self,
        _repository: &Repository,
        _ctx: &mut ModelContext<Repository>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        Box::pin(async {})
    }

    fn on_files_updated(
        &mut self,
        repository: &Repository,
        update: &RepositoryUpdate,
        _ctx: &mut ModelContext<Repository>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'static>> {
        let tx = self.repository_update_tx.clone();
        let update = update.clone();
        let index_lock_path = repository.git_dir().join("index.lock");
        Box::pin(async move {
            if update.commit_updated && async_fs::metadata(&index_lock_path).await.is_ok() {
                return;
            }
            if update.commit_updated {
                let _ = tx.send(update).await;
            }
        })
    }
}

/// Git 提交历史左侧面板视图。
pub struct GitHistoryView {
    working_directories_model: ModelHandle<WorkingDirectoriesModel>,
    history_model: Option<ModelHandle<GitHistoryModel>>,
    repository_path: Option<PathBuf>,
    expanded_commits: HashSet<String>,
    commit_mouse_states: HashMap<String, MouseStateHandle>,
    file_mouse_states: HashMap<(String, String), MouseStateHandle>,
    fallback_mouse_state: MouseStateHandle,
    scroll_state: ClippedScrollStateHandle,
    refresh_button: ViewHandle<ActionButton>,
    load_more_button: ViewHandle<ActionButton>,
}

impl GitHistoryView {
    pub fn new(
        working_directories_model: ModelHandle<WorkingDirectoriesModel>,
        ctx: &mut ViewContext<Self>,
    ) -> Self {
        let refresh_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("", NakedTheme)
                .with_size(ButtonSize::Small)
                .with_icon(Icon::Refresh)
                .with_tooltip(crate::t!("git-history-refresh"))
                .on_click(|ctx| ctx.dispatch_typed_action(GitHistoryAction::Refresh))
        });
        let load_more_button = ctx.add_typed_action_view(|_| {
            ActionButton::new(crate::t!("git-history-load-more"), SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| ctx.dispatch_typed_action(GitHistoryAction::LoadMore))
        });

        Self {
            working_directories_model,
            history_model: None,
            repository_path: None,
            expanded_commits: HashSet::new(),
            commit_mouse_states: HashMap::new(),
            file_mouse_states: HashMap::new(),
            fallback_mouse_state: MouseStateHandle::default(),
            scroll_state: ClippedScrollStateHandle::default(),
            refresh_button,
            load_more_button,
        }
    }

    pub fn set_repository(
        &mut self,
        repository_path: Option<PathBuf>,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.repository_path == repository_path {
            return;
        }

        if let Some(history_model) = self.history_model.take() {
            ctx.unsubscribe_to_model(&history_model);
        }
        self.repository_path = repository_path.clone();
        self.expanded_commits.clear();
        self.commit_mouse_states.clear();
        self.file_mouse_states.clear();

        if let Some(repository_path) = repository_path {
            let history_model = self.working_directories_model.update(ctx, |model, ctx| {
                model.get_or_create_git_history_model(repository_path, ctx)
            });
            if let Some(history_model) = history_model {
                self.history_model = Some(history_model);
                self.sync_mouse_states(ctx);
                if let Some(history_model) = self.history_model.as_ref() {
                    ctx.subscribe_to_model(history_model, |view, _, _, ctx| {
                        view.sync_mouse_states(ctx);
                        view.load_expanded_commit_files(ctx);
                        ctx.notify();
                    });
                }
            }
        }

        ctx.notify();
    }

    fn sync_mouse_states(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(history_model) = self.history_model.clone() else {
            self.commit_mouse_states.clear();
            self.file_mouse_states.clear();
            return;
        };
        let model = history_model.as_ref(ctx);
        let commit_hashes: HashSet<String> = model
            .commits()
            .iter()
            .map(|commit| commit.hash.clone())
            .collect();
        self.commit_mouse_states
            .retain(|hash, _| commit_hashes.contains(hash));
        for hash in &commit_hashes {
            self.commit_mouse_states.entry(hash.clone()).or_default();
        }

        let mut file_keys = HashSet::new();
        for commit in model.commits() {
            if let Some(files) = model.commit_files(&commit.hash) {
                for file in files {
                    file_keys.insert((commit.hash.clone(), file.path.clone()));
                }
            }
        }
        self.file_mouse_states
            .retain(|key, _| file_keys.contains(key));
        for key in file_keys {
            self.file_mouse_states.entry(key).or_default();
        }
    }

    fn load_expanded_commit_files(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(history_model) = self.history_model.clone() else {
            return;
        };
        let model = history_model.as_ref(ctx);
        let hashes_to_load: Vec<String> = self
            .expanded_commits
            .iter()
            .filter(|hash| {
                model.commits().iter().any(|commit| &commit.hash == *hash)
                    && model.commit_files(hash).is_none()
                    && !model.is_loading_files(hash)
                    && model.commit_file_error(hash).is_none()
            })
            .cloned()
            .collect();

        for hash in hashes_to_load {
            history_model.update(ctx, |model, ctx| model.load_commit_files(hash, ctx));
        }
    }

    fn render_message(&self, message: String, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        Container::new(
            warpui::elements::Text::new_inline(message, appearance.ui_font_family(), 12.)
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
        )
        .with_horizontal_padding(12.)
        .with_vertical_padding(20.)
        .finish()
    }

    fn render_header(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let repository = self
            .repository_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default();
        let title = warpui::elements::Text::new_inline(
            crate::t!("git-history-title"),
            appearance.ui_font_family(),
            14.,
        )
        .with_color(
            appearance
                .theme()
                .main_text_color(appearance.theme().background())
                .into(),
        )
        .finish();
        let repository =
            warpui::elements::Text::new_inline(repository, appearance.ui_font_family(), 11.)
                .with_color(
                    appearance
                        .theme()
                        .sub_text_color(appearance.theme().background())
                        .into(),
                )
                .finish();
        let labels = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(title)
            .with_child(repository)
            .finish();

        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Shrinkable::new(1., labels).finish())
                .with_child(ChildView::new(&self.refresh_button).finish())
                .finish(),
        )
        .with_horizontal_padding(8.)
        .with_vertical_padding(8.)
        .finish()
    }

    fn render_commit_row(
        &self,
        commit: &GitHistoryCommit,
        model: &GitHistoryModel,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let expanded = self.expanded_commits.contains(&commit.hash);

        let subject = warpui::elements::Text::new_inline(
            commit.subject.clone(),
            appearance.ui_font_family(),
            13.,
        )
        .with_color(theme.main_text_color(theme.background()).into())
        .finish();
        let short_hash = &commit.short_hash;
        let author = &commit.author;
        let committed_at = &commit.committed_at;
        let metadata = warpui::elements::Text::new_inline(
            format!("{short_hash} · {author} · {committed_at}"),
            appearance.ui_font_family(),
            11.,
        )
        .with_color(theme.sub_text_color(theme.background()).into())
        .finish();
        let summary = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(subject)
            .with_child(metadata)
            .finish();
        let hash = commit.hash.clone();
        let commit_mouse_state = self
            .commit_mouse_states
            .get(&commit.hash)
            .cloned()
            .unwrap_or_else(|| self.fallback_mouse_state.clone());
        let clickable_summary = Hoverable::new(commit_mouse_state, move |_| {
            Container::new(summary)
                .with_horizontal_padding(8.)
                .with_vertical_padding(6.)
                .finish()
        })
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GitHistoryAction::ToggleCommit { hash: hash.clone() });
        })
        .with_cursor(Cursor::PointingHand)
        .finish();

        let mut commit_column = Flex::column()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(clickable_summary);
        if expanded {
            if let Some(files) = model.commit_files(&commit.hash) {
                if files.is_empty() {
                    commit_column.add_child(
                        self.render_message(crate::t!("git-history-no-changed-files"), app),
                    );
                } else {
                    for file in files {
                        let path = file.path.clone();
                        let additions = file.additions;
                        let deletions = file.deletions;
                        let stats = format!("{path}  +{additions}  -{deletions}");
                        let repo_path = self.repository_path.clone();
                        let commit_hash = commit.hash.clone();
                        let commit_subject = commit.subject.clone();
                        let file_row = warpui::elements::Text::new_inline(
                            stats,
                            appearance.ui_font_family(),
                            11.,
                        )
                        .with_color(theme.sub_text_color(theme.background()).into())
                        .finish();
                        let file_mouse_state = self
                            .file_mouse_states
                            .get(&(commit.hash.clone(), path.clone()))
                            .cloned()
                            .unwrap_or_else(|| self.fallback_mouse_state.clone());
                        let clickable_file = Hoverable::new(file_mouse_state, move |mouse_state| {
                            let mut container = Container::new(file_row)
                                .with_padding_left(20.)
                                .with_padding_right(8.)
                                .with_padding_bottom(4.);
                            if mouse_state.is_hovered() {
                                container = container.with_background(theme.surface_2());
                            }
                            container.finish()
                        })
                        .on_click(move |ctx, _, _| {
                            if let Some(repo_path) = repo_path.clone() {
                                ctx.emit(GitHistoryEvent::OpenFileDiff {
                                    repo_path,
                                    commit_hash: commit_hash.clone(),
                                    commit_subject: commit_subject.clone(),
                                    file_path: path.clone(),
                                });
                            }
                        })
                        .with_cursor(Cursor::PointingHand)
                        .finish();
                        commit_column.add_child(clickable_file);
                    }
                }
            } else if let Some(error) = model.commit_file_error(&commit.hash) {
                let error_prefix = crate::t!("git-history-files-load-error");
                commit_column
                    .add_child(self.render_message(format!("{error_prefix}: {error}"), app));
            } else if model.is_loading_files(&commit.hash) {
                commit_column
                    .add_child(self.render_message(crate::t!("git-history-loading-files"), app));
            }
        }

        Container::new(commit_column.finish())
            .with_border(Border::bottom(1.).with_border_fill(theme.surface_3()))
            .with_corner_radius(CornerRadius::with_all(warpui::elements::Radius::Pixels(4.)))
            .finish()
    }
}

impl TypedActionView for GitHistoryView {
    type Action = GitHistoryAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        let Some(history_model) = self.history_model.as_ref() else {
            return;
        };

        match action {
            GitHistoryAction::Refresh => {
                history_model.update(ctx, |model, ctx| model.refresh(ctx));
            }
            GitHistoryAction::LoadMore => {
                history_model.update(ctx, |model, ctx| model.load_more(ctx));
            }
            GitHistoryAction::ToggleCommit { hash } => {
                if !self.expanded_commits.insert(hash.clone()) {
                    self.expanded_commits.remove(hash);
                } else {
                    let hash = hash.clone();
                    history_model.update(ctx, |model, ctx| model.load_commit_files(hash, ctx));
                }
                ctx.notify();
            }
        }
    }
}

impl View for GitHistoryView {
    fn ui_name() -> &'static str {
        "GitHistoryView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let header = self.render_header(app);
        let mut content = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(header);

        let Some(history_model) = self.history_model.as_ref() else {
            content.add_child(
                Shrinkable::new(
                    1.,
                    self.render_message(crate::t!("git-history-no-local-repository"), app),
                )
                .finish(),
            );
            return Container::new(content.finish()).finish();
        };

        let model = history_model.as_ref(app);
        if model.commits().is_empty() {
            let message = if model.is_loading() {
                crate::t!("git-history-loading")
            } else if let Some(error) = model.error() {
                let error_prefix = crate::t!("git-history-load-error");
                format!("{error_prefix}: {error}")
            } else {
                crate::t!("git-history-empty")
            };
            content.add_child(Shrinkable::new(1., self.render_message(message, app)).finish());
        } else {
            let mut rows = Flex::column().with_main_axis_size(MainAxisSize::Min);
            for commit in model.commits() {
                rows.add_child(self.render_commit_row(commit, model, app));
            }
            let appearance = Appearance::as_ref(app);
            let theme = appearance.theme();
            let scrollable = ClippedScrollable::vertical(
                self.scroll_state.clone(),
                Container::new(rows.finish()).finish(),
                ScrollbarWidth::Auto,
                theme.nonactive_ui_detail().into(),
                theme.active_ui_detail().into(),
                warpui::elements::Fill::None,
            )
            .with_overlayed_scrollbar()
            .finish();
            content.add_child(Shrinkable::new(1., scrollable).finish());

            if model.has_more() {
                content.add_child(
                    Container::new(ChildView::new(&self.load_more_button).finish())
                        .with_horizontal_padding(8.)
                        .with_vertical_padding(8.)
                        .finish(),
                );
            }
            if let Some(error) = model.error() {
                let error_prefix = crate::t!("git-history-load-error");
                content.add_child(self.render_message(format!("{error_prefix}: {error}"), app));
            } else if model.is_loading() {
                content.add_child(self.render_message(crate::t!("git-history-loading"), app));
            }
        }

        Container::new(content.finish())
            .with_horizontal_padding(4.)
            .finish()
    }
}
