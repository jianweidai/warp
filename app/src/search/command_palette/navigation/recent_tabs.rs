//! Cmd+E「最近打开」面板的数据源：把终端会话和编辑器里打开过的文件按最近使用时间合并展示。

use chrono::{Local, NaiveDateTime};
use fuzzy_match::FuzzyMatchResult;
use instant::Instant;
use itertools::Itertools;
use warpui::{AppContext, Entity, ModelHandle, SingletonEntity};

use crate::code::opened_files::OpenedFilesModel;
use crate::search::command_palette::files::search_item::FileSearchItem;
use crate::search::command_palette::mixer::CommandPaletteItemAction;
use crate::search::command_palette::navigation::search::filter_sessions;
use crate::search::command_palette::navigation::search_item::SearchItem;
use crate::search::data_source::{Query, QueryResult};
use crate::search::files::model::FileSearchModel;
use crate::search::mixer::DataSourceRunErrorWrapper;
use crate::search::SyncDataSource;
use crate::session_management::{SessionNavigationData, SessionSource};

/// 最多展示的文件数，避免长时间使用后列表过长。
const MAX_RECENT_FILES: usize = 50;

pub struct RecentTabsDataSource {
    session_source_handle: ModelHandle<SessionSource>,
}

impl RecentTabsDataSource {
    pub fn new(session_source_handle: ModelHandle<SessionSource>) -> Self {
        Self {
            session_source_handle,
        }
    }

    fn session_results(
        &self,
        search_term: &str,
        app: &AppContext,
    ) -> Vec<(Option<NaiveDateTime>, QueryResult<CommandPaletteItemAction>)> {
        let active_session_id = match self.session_source_handle.as_ref(app) {
            SessionSource::None => None,
            SessionSource::Set { active_pane_id, .. } => Some(*active_pane_id),
        };
        let sessions = SessionNavigationData::all_sessions(app).collect_vec();
        filter_sessions(sessions.iter(), search_term)
            .map(|matched| {
                let focused_at = matched.session.last_focus_ts();
                (
                    focused_at,
                    SearchItem::new(matched, active_session_id).into(),
                )
            })
            .collect()
    }

    fn file_results(
        search_term: &str,
        app: &AppContext,
    ) -> Vec<(Option<NaiveDateTime>, QueryResult<CommandPaletteItemAction>)> {
        let now = Instant::now();
        let now_local = Local::now().naive_local();
        OpenedFilesModel::as_ref(app)
            .all_opened_files()
            .sorted_by_key(|(_, _, opened_at)| std::cmp::Reverse(**opened_at))
            .take(MAX_RECENT_FILES)
            // 跳过已被删除或移动的文件。
            .filter(|(repo_path, file_path, _)| repo_path.join(file_path).is_file())
            .filter_map(|(repo_path, file_path, opened_at)| {
                let match_result: FuzzyMatchResult =
                    FileSearchModel::fuzzy_match_path(&file_path.to_string_lossy(), search_term)?;
                // 会话用本地时间记录聚焦时刻，这里把 Instant 换算成同一时间轴以便合并排序。
                let opened_at = chrono::Duration::from_std(now.duration_since(*opened_at))
                    .ok()
                    .map(|elapsed| now_local - elapsed);
                let item = FileSearchItem {
                    path: file_path.clone(),
                    project_directory: repo_path.to_string_lossy().to_string(),
                    match_result,
                    line_and_column_arg: None,
                    is_directory: false,
                };
                Some((opened_at, item.into()))
            })
            .collect()
    }
}

impl SyncDataSource for RecentTabsDataSource {
    type Action = CommandPaletteItemAction;

    fn run_query(
        &self,
        query: &Query,
        app: &AppContext,
    ) -> Result<Vec<QueryResult<Self::Action>>, DataSourceRunErrorWrapper> {
        let search_term = query.text.trim().to_lowercase();
        let mut results = self.session_results(&search_term, app);
        results.extend(Self::file_results(&search_term, app));
        // 与会话面板一致：按时间升序输出，由面板负责把最近的放在最前。
        results.sort_by_key(|(timestamp, _)| *timestamp);
        Ok(results.into_iter().map(|(_, result)| result).collect())
    }
}

impl Entity for RecentTabsDataSource {
    type Event = ();
}
