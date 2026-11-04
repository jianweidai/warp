//! 用 LSP 格式化当前文件（Shift+Alt+F），可选在保存时格式化（默认关闭）。
//!
//! - 先把还没发出的编辑同步给服务器，再请求格式化，避免按旧内容格式化。
//! - 服务器超过 [`FORMAT_TIMEOUT`] 没有返回就放弃，保存照常进行。
//! - 等待期间内容又被修改时，丢弃格式化结果，避免改乱新内容。

use std::ops::Range;
use std::time::Duration;

use ::settings::Setting;
use futures::future::{select, Either};
use lsp::types::TextEdit;
use lsp::FormattingOptions;
use string_offset::CharOffset;
use vec1::Vec1;
use warp_editor::content::text::IndentUnit;
use warp_util::file::FileId;
use warpui::r#async::Timer;
use warpui::{SingletonEntity, ViewContext};

use super::LocalCodeEditorView;
use crate::code::global_buffer_model::GlobalBufferModel;
use crate::settings::CodeSettings;

/// 等待格式化结果的最长时间。
const FORMAT_TIMEOUT: Duration = Duration::from_secs(3);

impl LocalCodeEditorView {
    /// 手动格式化当前文件。
    pub(super) fn format_document(&mut self, ctx: &mut ViewContext<Self>) {
        self.request_format(None, ctx);
    }

    /// 保存。开启了“保存时格式化”且服务器可用时先格式化，否则直接保存。
    pub(super) fn format_then_save(&mut self, file_id: FileId, ctx: &mut ViewContext<Self>) {
        // 没有语言服务器时不读设置，直接保存。
        let format_on_save =
            self.lsp_server.is_some() && *CodeSettings::as_ref(ctx).format_on_save.value();
        if !format_on_save || !self.request_format(Some(file_id), ctx) {
            self.perform_save(file_id, ctx);
        }
    }

    /// 发出格式化请求，返回是否已发出。`save` 不为空时，结果回来后（无论成败）再保存。
    fn request_format(&mut self, save: Option<FileId>, ctx: &mut ViewContext<Self>) -> bool {
        let (Some(server), Some(path), Some(file_id)) =
            (self.lsp_server.as_ref(), self.file_path(), self.file_id())
        else {
            return false;
        };
        if !server.as_ref(ctx).is_ready_for_requests() {
            return false;
        }

        let (tab_size, insert_spaces) = match self.editor.as_ref(ctx).indent_unit(ctx) {
            IndentUnit::Tab => (4, false),
            IndentUnit::Space(width) => (width as u32, true),
        };
        let options = FormattingOptions {
            tab_size,
            insert_spaces,
            properties: Default::default(),
            trim_trailing_whitespace: None,
            insert_final_newline: None,
            trim_final_newlines: None,
        };
        let format = match server
            .as_ref(ctx)
            .format_document(path.to_path_buf(), options)
        {
            Ok(future) => future,
            Err(e) => {
                log::warn!("Failed to request LSP formatting: {e}");
                return false;
            }
        };

        let flush = GlobalBufferModel::handle(ctx)
            .update(ctx, |model, ctx| model.take_pending_lsp_sync(file_id, ctx));
        let version = self.editor.as_ref(ctx).buffer_version(ctx).as_usize();
        let request = async move {
            if let Some(flush) = flush {
                flush.await;
            }
            let timeout = async {
                Timer::after(FORMAT_TIMEOUT).await;
            };
            match select(Box::pin(format), Box::pin(timeout)).await {
                Either::Left((result, _)) => Some(result),
                Either::Right(_) => None,
            }
        };

        ctx.spawn(request, move |me, result, ctx| {
            match result {
                Some(Ok(Some(edits))) => me.apply_format_edits(edits, version, ctx),
                Some(Ok(None)) => log::debug!("LSP server returned no formatting edits"),
                Some(Err(e)) => log::warn!("LSP formatting failed: {e}"),
                None => log::warn!("LSP formatting timed out"),
            }
            if let Some(file_id) = save {
                me.perform_save(file_id, ctx);
            }
        });
        true
    }

    fn apply_format_edits(
        &mut self,
        edits: Vec<TextEdit>,
        version: usize,
        ctx: &mut ViewContext<Self>,
    ) {
        self.editor.update(ctx, |editor, ctx| {
            if editor.buffer_version(ctx).as_usize() != version {
                log::debug!("Discarding LSP formatting: buffer changed while waiting");
                return;
            }
            let mut converted: Vec<(String, Range<CharOffset>)> = edits
                .into_iter()
                .filter_map(|edit| {
                    let start = editor.lsp_location_to_offset(&edit.range.start, ctx);
                    let end = editor.lsp_location_to_offset(&edit.range.end, ctx);
                    (start <= end).then_some((edit.text, start..end))
                })
                .collect();
            // 从后往前应用，前面的偏移不受影响。
            converted.sort_by(|a, b| b.1.start.cmp(&a.1.start));
            if let Ok(edits) = Vec1::try_from_vec(converted) {
                editor.apply_edits(edits, ctx);
            }
        });
    }
}
