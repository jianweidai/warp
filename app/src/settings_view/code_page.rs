//! Code 设置页：「编辑器与代码评审」相关的本地开关，以及各语言服务器的开关和状态
//! （见 `code_page_lsp.rs`）。
//!
//! 旧版的 LSP 管理子页和 codebase indexing 已下线，`Code` 在侧边栏是单层 Page。

#[cfg(feature = "local_fs")]
use super::features::external_editor::ExternalEditorView;
use super::{
    settings_page::{
        render_body_item, MatchData, PageType, SettingsPageMeta, SettingsPageViewHandle,
        SettingsWidget,
    },
    LocalOnlyIconState, SettingsAction, SettingsSection, ToggleState,
};
use crate::{
    appearance::Appearance, send_telemetry_from_ctx, settings::CodeSettings,
    terminal::general_settings::GeneralSettings, workspace::tab_settings::TabSettings,
    TelemetryEvent,
};
use ai::project_context::model::{ProjectContextModel, ProjectContextModelEvent};

use std::path::PathBuf;
use warp_core::{
    features::FeatureFlag,
    report_if_error,
    settings::{Setting as _, ToggleableSetting as _},
};

#[cfg(all(
    not(target_family = "wasm"),
    feature = "local_fs",
    feature = "local_tty"
))]
#[path = "code_page_lsp.rs"]
mod lsp_settings;
use warpui::{
    elements::{ChildView, Element, Empty},
    keymap::ContextPredicate,
    ui_components::{components::UiComponent, switch::SwitchStateHandle},
    Action, AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

pub struct CodeSettingsPageView {
    page: PageType<Self>,
    #[cfg(feature = "local_fs")]
    external_editor_view: Option<ViewHandle<ExternalEditorView>>,
}

impl CodeSettingsPageView {
    pub fn new(ctx: &mut ViewContext<CodeSettingsPageView>) -> Self {
        // 订阅 ProjectContextModel:project rules 变动时重渲染,
        // 让任何依赖 rule 集合的子组件保持最新。
        ctx.subscribe_to_model(&ProjectContextModel::handle(ctx), |_me, _, event, ctx| {
            if matches!(event, ProjectContextModelEvent::KnownRulesChanged(_)) {
                ctx.notify();
            }
        });

        // 语言服务器的安装、运行状态变化时刷新开关下方的说明。
        #[cfg(all(
            not(target_family = "wasm"),
            feature = "local_fs",
            feature = "local_tty"
        ))]
        ctx.subscribe_to_model(
            &crate::code::lsp_lifecycle::LspLifecycle::handle(ctx),
            |_me, _, _event, ctx| ctx.notify(),
        );

        let (page, external_editor_view) = Self::build_page(ctx);

        Self {
            page,
            #[cfg(feature = "local_fs")]
            external_editor_view,
        }
    }

    /// 构造页面 widgets。Code 现在是单页(无子页面、无 category 标题),
    /// 直接铺平展示「编辑器与代码评审」开关。
    #[cfg(feature = "local_fs")]
    fn build_page(
        ctx: &mut ViewContext<Self>,
    ) -> (PageType<Self>, Option<ViewHandle<ExternalEditorView>>) {
        let (widgets, external_editor_view) = if FeatureFlag::ZapNewSettingsModes.is_enabled() {
            let editor_view = ctx.add_typed_action_view(ExternalEditorView::new);
            #[allow(unused_mut)]
            let mut widgets: Vec<Box<dyn SettingsWidget<View = Self>>> = vec![
                Box::new(ExternalEditorCodeWidget),
                Box::new(AutoOpenCodeReviewPaneCodeWidget::default()),
                Box::new(CodeReviewPanelToggleWidget::default()),
                Box::new(CodeReviewDiffStatsToggleWidget::default()),
                Box::new(ProjectExplorerToggleWidget::default()),
                Box::new(GlobalSearchToggleWidget::default()),
            ];
            #[cfg(all(
                not(target_family = "wasm"),
                feature = "local_fs",
                feature = "local_tty"
            ))]
            widgets.extend(lsp_settings::widgets());
            (widgets, Some(editor_view))
        } else {
            // legacy 视图:旧设置模式下 Code 页不渲染任何内容(原 CodePageWidget
            // 仅渲染一个 LSP 时代的 header,无实际意义,直接返回空页面)。
            (vec![], None)
        };
        (
            PageType::new_uncategorized(widgets, None),
            external_editor_view,
        )
    }

    /// wasm 构建下没有 ExternalEditorView,只渲染 4 个非外部编辑器开关。
    #[cfg(not(feature = "local_fs"))]
    fn build_page(
        _ctx: &mut ViewContext<Self>,
    ) -> (PageType<Self>, Option<ViewHandle<ExternalEditorView>>) {
        let widgets: Vec<Box<dyn SettingsWidget<View = Self>>> =
            if FeatureFlag::ZapNewSettingsModes.is_enabled() {
                vec![
                    Box::new(AutoOpenCodeReviewPaneCodeWidget::default()),
                    Box::new(CodeReviewPanelToggleWidget::default()),
                    Box::new(CodeReviewDiffStatsToggleWidget::default()),
                    Box::new(ProjectExplorerToggleWidget::default()),
                    Box::new(GlobalSearchToggleWidget::default()),
                ]
            } else {
                vec![]
            };
        (PageType::new_uncategorized(widgets, None), None)
    }
}

impl Entity for CodeSettingsPageView {
    type Event = CodeSettingsPageEvent;
}

impl View for CodeSettingsPageView {
    fn ui_name() -> &'static str {
        "CodePage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

#[derive(Debug, Clone)]
pub enum CodeSettingsPageEvent {
    OpenProjectRules { rule_paths: Vec<PathBuf> },
}

#[derive(Debug, Clone)]
pub enum CodeSettingsPageAction {
    OpenProjectRules {
        rule_paths: Vec<PathBuf>,
    },
    ToggleCodeReviewPanel,
    ToggleShowCodeReviewDiffStats,
    ToggleAutoOpenCodeReviewPane,
    ToggleProjectExplorer,
    ToggleGlobalSearch,
    /// 启用或关闭某个语言服务器。
    ToggleLspServer(lsp::supported_servers::LSPServerType),
}

impl TypedActionView for CodeSettingsPageView {
    type Action = CodeSettingsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            CodeSettingsPageAction::OpenProjectRules { rule_paths } => {
                ctx.emit(CodeSettingsPageEvent::OpenProjectRules {
                    rule_paths: rule_paths.clone(),
                });
            }
            CodeSettingsPageAction::ToggleCodeReviewPanel => {
                TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.show_code_review_button.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            CodeSettingsPageAction::ToggleShowCodeReviewDiffStats => {
                TabSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .show_code_review_diff_stats
                        .toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            CodeSettingsPageAction::ToggleProjectExplorer => {
                CodeSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.show_project_explorer.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            CodeSettingsPageAction::ToggleLspServer(server) => {
                let server = *server;
                CodeSettings::handle(ctx).update(ctx, |settings, ctx| {
                    let mut enabled = settings.enabled_lsp_servers.value().clone();
                    if enabled.contains(&server) {
                        enabled.retain(|existing| *existing != server);
                    } else {
                        enabled.push(server);
                    }
                    report_if_error!(settings.enabled_lsp_servers.set_value(enabled, ctx));
                });
                ctx.notify();
            }
            CodeSettingsPageAction::ToggleGlobalSearch => {
                CodeSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.show_global_search.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
            CodeSettingsPageAction::ToggleAutoOpenCodeReviewPane => {
                GeneralSettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings
                        .auto_open_code_review_pane_on_first_agent_change
                        .toggle_and_save_value(ctx));
                });
                send_telemetry_from_ctx!(
                    TelemetryEvent::FeaturesPageAction {
                        action: "ToggleAutoOpenCodeReviewPane".to_string(),
                        value: format!(
                            "{}",
                            *GeneralSettings::as_ref(ctx)
                                .auto_open_code_review_pane_on_first_agent_change
                        )
                    },
                    ctx
                );
                ctx.notify();
            }
        }
    }
}

pub fn init_actions_from_parent_view<T: Action + Clone>(
    _app: &mut AppContext,
    _context: &ContextPredicate,
    _builder: fn(SettingsAction) -> T,
) {
}

#[cfg(feature = "local_fs")]
struct ExternalEditorCodeWidget;

#[cfg(feature = "local_fs")]
impl SettingsWidget for ExternalEditorCodeWidget {
    type View = CodeSettingsPageView;

    fn search_terms(&self) -> &str {
        "code editor open files markdown AI conversations layout pane tab"
    }

    fn render(
        &self,
        view: &Self::View,
        _appearance: &Appearance,
        _app: &AppContext,
    ) -> Box<dyn Element> {
        if let Some(editor_view) = &view.external_editor_view {
            ChildView::new(editor_view).finish()
        } else {
            Empty::new().finish()
        }
    }
}

#[derive(Default)]
struct AutoOpenCodeReviewPaneCodeWidget {
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for AutoOpenCodeReviewPaneCodeWidget {
    type View = CodeSettingsPageView;

    fn search_terms(&self) -> &str {
        "oz auto open code review pane panel agent mode change first time accepted diff view conversation"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let general_settings = GeneralSettings::as_ref(app);
        render_body_item::<CodeSettingsPageAction>(
            crate::t!("settings-code-auto-open-review-panel"),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*general_settings.auto_open_code_review_pane_on_first_agent_change)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(CodeSettingsPageAction::ToggleAutoOpenCodeReviewPane);
                })
                .finish(),
            Some(crate::t!("settings-code-auto-open-review-panel-desc")),
        )
    }
}

impl SettingsPageMeta for CodeSettingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::Code
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        FeatureFlag::ZapNewSettingsModes.is_enabled()
    }

    fn on_page_selected(&mut self, _: bool, _ctx: &mut ViewContext<Self>) {
        // 打开设置页时检查各语言服务器是否已安装（只检查，不下载）。
        #[cfg(all(
            not(target_family = "wasm"),
            feature = "local_fs",
            feature = "local_tty"
        ))]
        crate::code::lsp_lifecycle::LspLifecycle::handle(_ctx)
            .update(_ctx, |lifecycle, ctx| lifecycle.check_installed(ctx));
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<CodeSettingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<CodeSettingsPageView>) -> Self {
        SettingsPageViewHandle::Code(view_handle)
    }
}

#[derive(Default)]
struct CodeReviewPanelToggleWidget {
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for CodeReviewPanelToggleWidget {
    type View = CodeSettingsPageView;

    fn search_terms(&self) -> &str {
        "code review panel right side diff git"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let tab_settings = TabSettings::as_ref(app);

        render_body_item::<CodeSettingsPageAction>(
            crate::t!("settings-code-show-code-review-button"),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*tab_settings.show_code_review_button)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(CodeSettingsPageAction::ToggleCodeReviewPanel);
                })
                .finish(),
            Some(crate::t!("settings-code-show-code-review-button-desc")),
        )
    }
}

#[derive(Default)]
struct CodeReviewDiffStatsToggleWidget {
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for CodeReviewDiffStatsToggleWidget {
    type View = CodeSettingsPageView;

    fn search_terms(&self) -> &str {
        "code review diff stats lines added removed counts"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let tab_settings = TabSettings::as_ref(app);

        render_body_item::<CodeSettingsPageAction>(
            crate::t!("settings-code-show-diff-stats"),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*tab_settings.show_code_review_diff_stats)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(
                        CodeSettingsPageAction::ToggleShowCodeReviewDiffStats,
                    );
                })
                .finish(),
            Some(crate::t!("settings-code-show-diff-stats-desc")),
        )
    }
}

#[derive(Default)]
struct ProjectExplorerToggleWidget {
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for ProjectExplorerToggleWidget {
    type View = CodeSettingsPageView;

    fn search_terms(&self) -> &str {
        "project explorer file tree left panel tools"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let code_settings = CodeSettings::as_ref(app);

        render_body_item::<CodeSettingsPageAction>(
            crate::t!("settings-code-project-explorer"),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*code_settings.show_project_explorer)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(CodeSettingsPageAction::ToggleProjectExplorer);
                })
                .finish(),
            Some(crate::t!("settings-code-project-explorer-desc")),
        )
    }
}

#[derive(Default)]
struct GlobalSearchToggleWidget {
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for GlobalSearchToggleWidget {
    type View = CodeSettingsPageView;

    fn search_terms(&self) -> &str {
        "global search file search left panel tools"
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let code_settings = CodeSettings::as_ref(app);

        render_body_item::<CodeSettingsPageAction>(
            crate::t!("settings-code-global-search"),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(*code_settings.show_global_search)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(CodeSettingsPageAction::ToggleGlobalSearch);
                })
                .finish(),
            Some(crate::t!("settings-code-global-search-desc")),
        )
    }
}
