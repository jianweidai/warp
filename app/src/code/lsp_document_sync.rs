//! 把本地缓冲区的内容同步给 LSP 服务器：didOpen、didChange、didClose。
//!
//! - 只处理 [`BufferSource::Local`]，远程文件和 ServerLocal 不处理。
//! - 只在调用过 [`GlobalBufferModel::subscribe_to_lsp_manager`] 后生效。remote-server
//!   daemon 不调用它，也不注册 `LspManagerModel`。
//! - 连续编辑在 [`DID_CHANGE_DEBOUNCE`] 内合并，最后一次发送整份内容。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::stream::AbortHandle;
use futures_util::FutureExt;
use lsp::types::TextDocumentContentChangeEvent;
use lsp::{LspManagerModel, LspManagerModelEvent, LspServerModel};
use warp_editor::content::buffer::{Buffer, BufferEvent};
use warp_util::file::FileId;
use warpui::r#async::Timer;
use warpui::{ModelContext, ModelHandle, SingletonEntity};

use super::{BufferLocation, BufferSource, GlobalBufferModel};

/// 停止编辑多久后才把内容发给服务器。
const DID_CHANGE_DEBOUNCE: Duration = Duration::from_millis(300);

/// 等待发送的内容同步，按文件记录，便于取消。
#[derive(Default)]
pub(super) struct LspSync {
    pending: HashMap<FileId, AbortHandle>,
}

impl GlobalBufferModel {
    /// 客户端 app 专用：开启 LSP 文档同步。服务器启动后，把它负责的已加载文件发过去。
    pub fn subscribe_to_lsp_manager(&mut self, ctx: &mut ModelContext<Self>) {
        self.lsp_sync = Some(LspSync::default());
        ctx.subscribe_to_model(&LspManagerModel::handle(ctx), |me, event, ctx| {
            if let LspManagerModelEvent::ServerStarted(root) = event {
                me.lsp_open_buffers_under(root, ctx);
            }
        });
    }

    /// 监听本地缓冲区的内容变化。
    pub(super) fn lsp_watch_buffer(
        &mut self,
        file_id: FileId,
        buffer: &ModelHandle<Buffer>,
        ctx: &mut ModelContext<Self>,
    ) {
        if self.lsp_sync.is_none() {
            return;
        }
        ctx.subscribe_to_model(buffer, move |me, event, ctx| {
            if matches!(event, BufferEvent::ContentChanged { .. }) {
                me.lsp_schedule_sync(file_id, ctx);
            }
        });
    }

    /// 文件关闭、重命名或缓冲区释放前调用，通知服务器关闭文档。
    pub(super) fn lsp_close(&mut self, file_id: FileId, ctx: &mut ModelContext<Self>) {
        let Some(sync) = self.lsp_sync.as_mut() else {
            return;
        };
        if let Some(pending) = sync.pending.remove(&file_id) {
            pending.abort();
        }
        let Some(path) = self.lsp_local_path(file_id) else {
            return;
        };
        let Some(server) = ready_server_for_path(&path, ctx) else {
            return;
        };
        if !server.as_ref(ctx).document_is_open(&path).unwrap_or(false) {
            return;
        }
        let future = match server.as_ref(ctx).did_close_document(path) {
            Ok(future) => future,
            Err(e) => {
                log::warn!("Failed to close document with LSP server: {e}");
                return;
            }
        };
        ctx.spawn(future, |_, _, _| {});
    }

    /// 内容变化后延迟同步。没有对应的服务器时不做任何事。
    fn lsp_schedule_sync(&mut self, file_id: FileId, ctx: &mut ModelContext<Self>) {
        if self.lsp_sync.is_none() || !self.lsp_is_loaded_local(file_id) {
            return;
        }
        let Some(path) = self.lsp_local_path(file_id) else {
            return;
        };
        if ready_server_for_path(&path, ctx).is_none() {
            return;
        }

        let timer = async {
            Timer::after(DID_CHANGE_DEBOUNCE).await;
        };
        let handle = ctx.spawn(timer, move |me, _, ctx| {
            if let Some(sync) = me.lsp_sync.as_mut() {
                sync.pending.remove(&file_id);
            }
            me.lsp_sync_now(file_id, ctx);
        });
        if let Some(sync) = self.lsp_sync.as_mut() {
            if let Some(previous) = sync.pending.insert(file_id, handle.abort_handle()) {
                previous.abort();
            }
        }
    }

    /// 立即把整份内容发给服务器：文档还没打开就 didOpen，否则 didChange。
    pub(super) fn lsp_sync_now(&mut self, file_id: FileId, ctx: &mut ModelContext<Self>) {
        if let Some(future) = self.lsp_sync_future(file_id, ctx) {
            ctx.spawn(future, |_, _, _| {});
        }
    }

    /// 如果有还没发出的编辑，取消延迟发送，返回立即发送的 future。
    ///
    /// 格式化等请求要先等它完成，确保服务器看到的是最新内容。
    pub fn take_pending_lsp_sync(
        &mut self,
        file_id: FileId,
        ctx: &mut ModelContext<Self>,
    ) -> Option<BoxFuture<'static, ()>> {
        let pending = self.lsp_sync.as_mut()?.pending.remove(&file_id)?;
        pending.abort();
        self.lsp_sync_future(file_id, ctx)
    }

    fn lsp_sync_future(
        &mut self,
        file_id: FileId,
        ctx: &mut ModelContext<Self>,
    ) -> Option<BoxFuture<'static, ()>> {
        if self.lsp_sync.is_none() || !self.lsp_is_loaded_local(file_id) {
            return None;
        }
        let path = self.lsp_local_path(file_id)?;
        let buffer = self
            .buffers
            .get(&file_id)
            .and_then(|state| state.buffer.upgrade(ctx))?;
        let server = ready_server_for_path(&path, ctx)?;

        let content = buffer.as_ref(ctx).text_with_line_ending().into_string();
        let version = buffer.as_ref(ctx).buffer_version().as_usize();
        let server = server.as_ref(ctx);
        let result = if server.document_is_open(&path).unwrap_or(false) {
            let change = TextDocumentContentChangeEvent {
                range: None,
                text: content,
            };
            server
                .did_change_document(path, version.into(), vec![change])
                .map(|future| future.boxed())
        } else {
            server
                .did_open_document(path, content, version)
                .map(|future| future.boxed())
        };
        match result {
            Ok(future) => Some(
                future
                    .map(|result| {
                        if let Err(e) = result {
                            log::warn!("Failed to sync document with LSP server: {e}");
                        }
                    })
                    .boxed(),
            ),
            Err(e) => {
                log::warn!("Failed to sync document with LSP server: {e}");
                None
            }
        }
    }

    /// 服务器启动后，把它负责范围内已加载的文件发过去。
    fn lsp_open_buffers_under(&mut self, root: &Path, ctx: &mut ModelContext<Self>) {
        let file_ids: Vec<FileId> = self
            .location_to_id
            .iter()
            .filter_map(|(location, id)| match location {
                BufferLocation::Local(path) if path.starts_with(root) => Some(*id),
                _ => None,
            })
            .collect();
        for file_id in file_ids {
            self.lsp_sync_now(file_id, ctx);
        }
    }

    fn lsp_local_path(&self, file_id: FileId) -> Option<PathBuf> {
        match self.location_to_id.get_by_right(&file_id)? {
            BufferLocation::Local(path) => Some(path.clone()),
            _ => None,
        }
    }

    fn lsp_is_loaded_local(&self, file_id: FileId) -> bool {
        self.buffers.get(&file_id).is_some_and(|state| {
            matches!(
                state.source,
                BufferSource::Local {
                    base_content_version: Some(_)
                }
            )
        })
    }
}

fn ready_server_for_path(
    path: &Path,
    ctx: &ModelContext<GlobalBufferModel>,
) -> Option<ModelHandle<LspServerModel>> {
    LspManagerModel::as_ref(ctx)
        .server_for_path(path, ctx)
        .filter(|server| server.as_ref(ctx).is_ready_for_requests())
}
