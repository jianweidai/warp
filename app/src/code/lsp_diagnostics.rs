//! 在本地编辑器中显示 LSP 诊断（错误、警告的下划线）。
//!
//! 文件加载后连接负责它的服务器；服务器稍后才启动时，收到 `ServerStarted` 再连接。
//! 只显示与当前缓冲区版本一致的诊断，避免把旧结果画到新内容上。

use lsp::{DiagnosticSeverity, LspEvent, LspManagerModel, LspManagerModelEvent};
use string_offset::CharOffset;
use warp_core::ui::appearance::Appearance;
use warp_editor::render::model::Decoration;
use warpui::{SingletonEntity, ViewContext};

use super::LocalCodeEditorView;

impl LocalCodeEditorView {
    /// 文件加载完成后调用：连接服务器，并在服务器启动、停止时更新。
    pub(super) fn connect_lsp_diagnostics(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.subscribe_to_model(&LspManagerModel::handle(ctx), |me, _, event, ctx| {
            let Some(path) = me.file_path() else {
                return;
            };
            match event {
                LspManagerModelEvent::ServerStarted(root) if path.starts_with(root) => {
                    me.try_connect_lsp_server(ctx);
                }
                LspManagerModelEvent::ServerStopped(root) if path.starts_with(root) => {
                    me.set_diagnostics(Vec::new(), ctx);
                }
                LspManagerModelEvent::ServerRemoved { server_id, .. }
                    if me
                        .lsp_server
                        .as_ref()
                        .is_some_and(|server| server.as_ref(ctx).id() == *server_id) =>
                {
                    me.lsp_server = None;
                    me.set_diagnostics(Vec::new(), ctx);
                }
                _ => {}
            }
        });
        self.try_connect_lsp_server(ctx);
    }

    fn try_connect_lsp_server(&mut self, ctx: &mut ViewContext<Self>) {
        if self.lsp_server.is_some() {
            self.refresh_diagnostics(ctx);
            return;
        }
        let Some(path) = self.file_path() else {
            return;
        };
        let Some(server) = LspManagerModel::as_ref(ctx).server_for_path(path, ctx) else {
            return;
        };
        ctx.subscribe_to_model(&server, |me, _, event, ctx| {
            if let LspEvent::DiagnosticsUpdated { path } = event {
                if me.file_path() == Some(path.as_path()) {
                    me.refresh_diagnostics(ctx);
                }
            }
        });
        self.lsp_server = Some(server);
        self.refresh_diagnostics(ctx);
    }

    fn refresh_diagnostics(&mut self, ctx: &mut ViewContext<Self>) {
        let decorations = self.compute_diagnostic_decorations(ctx);
        self.set_diagnostics(decorations, ctx);
    }

    fn set_diagnostics(&mut self, decorations: Vec<Decoration>, ctx: &mut ViewContext<Self>) {
        self.editor.update(ctx, |editor, ctx| {
            editor.set_diagnostic_decorations(decorations, ctx);
        });
    }

    /// 只保留错误和警告，转换成虚线下划线。
    fn compute_diagnostic_decorations(&self, ctx: &ViewContext<Self>) -> Vec<Decoration> {
        let (Some(server), Some(path)) = (self.lsp_server.as_ref(), self.file_path()) else {
            return Vec::new();
        };
        let Some(document) = server.as_ref(ctx).diagnostics_for_path(path).ok().flatten() else {
            return Vec::new();
        };

        let editor = self.editor.as_ref(ctx);
        let current_version = editor.buffer_version(ctx).as_usize() as i32;
        if document
            .version
            .is_some_and(|version| version != current_version)
        {
            return Vec::new();
        }

        let theme = Appearance::as_ref(ctx).theme();
        let error_color = theme.ui_error_color();
        let warning_color = theme.ui_warning_color();
        let one = CharOffset::from(1);

        document
            .diagnostics
            .iter()
            .filter_map(|diagnostic| {
                let color = match diagnostic.severity? {
                    DiagnosticSeverity::ERROR => error_color,
                    DiagnosticSeverity::WARNING => warning_color,
                    _ => return None,
                };
                let range: lsp::types::Range = diagnostic.range.into();
                let mut start = editor.lsp_location_to_offset(&range.start, ctx);
                let mut end = editor.lsp_location_to_offset(&range.end, ctx);

                // 空范围至少画一个字符。
                if start >= end {
                    end = start + one;
                }
                // 指向行尾换行符的诊断（如缺少分号）改为画在前一个字符上，否则看不见。
                if end == start + one && editor.char_at(start, ctx) == Some('\n') && start > one {
                    start = start - one;
                    end = end - one;
                }

                // 缓冲区偏移从 1 开始，渲染偏移从 0 开始。
                Some(Decoration::new(start - one, end - one).with_dashed_underline(color))
            })
            .collect()
    }
}
