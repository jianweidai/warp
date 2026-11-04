//! LSP 服务器的按需启动和空闲回收。
//!
//! - 打开本地文件后，只有该语言的服务器在 `code.lsp.enabled_servers` 中才启动。
//!   默认集合为空，所以默认不会启动任何进程。
//! - 每个 git 仓库根目录一组服务器。不在 git 仓库内的文件不启动。
//! - 仓库内已没有打开的文件，并且超过 [`IDLE_GRACE_PERIOD`]，就停止该仓库的服务器。
//! - 在设置里关闭某个服务器后，立即停止并移除它。
//! - 记录每种服务器的安装情况和最近一次启动错误，供设置页显示；启动失败时弹出提示。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::settings::Setting;
use futures::stream::AbortHandle;
use lsp::supported_servers::LSPServerType;
use lsp::{
    CommandBuilder, LanguageId, LanguageServerCandidate, LanguageServerId, LspEvent,
    LspManagerModel, LspServerConfig, LspServerModel, LspState,
};
use repo_metadata::repositories::DetectedRepositories;
use sha2::{Digest, Sha256};
use warpui::r#async::Timer;
use warpui::windowing::WindowManager;
use warpui::{AppContext, Entity, ModelContext, ModelHandle, SingletonEntity};

use crate::channel::ChannelState;
use crate::code::local_code_editor::LocalCodeEditorView;
use crate::settings::{CodeSettings, CodeSettingsChangedEvent};
use crate::terminal::local_shell::LocalShellState;
use crate::view_components::DismissibleToast;
use crate::workspace::ToastStack;

/// 仓库内没有打开的文件后，再等多久停止服务器。
const IDLE_GRACE_PERIOD: Duration = Duration::from_secs(5 * 60);
/// 有服务器运行时，检查空闲的间隔。没有服务器运行时不检查。
const SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// 本地文件加载完成后调用。没有启用对应语言的服务器时直接返回，不做任何事。
///
/// 返回该文件是否启用了 LSP，调用方据此决定是否显示诊断。
pub fn on_local_file_loaded(path: &Path, ctx: &mut AppContext) -> bool {
    let servers = enabled_servers_for_path(path, ctx);
    if servers.is_empty() {
        return false;
    }
    LspLifecycle::handle(ctx).update(ctx, |lifecycle, ctx| {
        lifecycle.ensure_started(path, servers, ctx);
    });
    true
}

pub(crate) fn enabled_servers_for_path(path: &Path, ctx: &AppContext) -> Vec<LSPServerType> {
    let Some(language) = LanguageId::from_path(path) else {
        return Vec::new();
    };
    CodeSettings::as_ref(ctx)
        .enabled_lsp_servers
        .value()
        .iter()
        .copied()
        .filter(|server| server.languages().contains(&language))
        .collect()
}

/// 某种服务器的状态，供设置页显示。
#[derive(Clone, Debug, Default)]
pub struct ServerStatus {
    /// `None` 表示还没检查或正在检查。
    pub installed: Option<bool>,
    /// 最近一次启动失败的原因，成功启动后清除。
    pub last_error: Option<String>,
}

pub enum LspLifecycleEvent {
    /// 安装情况、启动错误或运行状态有变化。
    StatusChanged,
}

#[derive(Default)]
pub struct LspLifecycle {
    statuses: HashMap<LSPServerType, ServerStatus>,
    /// 已订阅事件的服务器，避免重复订阅。
    watched: HashSet<LanguageServerId>,
    checking_install: bool,
    /// 只在需要启动服务器时才创建，用于检查安装情况。
    http_client: Option<Arc<http_client::Client>>,
    /// 正在获取 PATH、尚未注册的服务器，避免重复启动。
    pending: HashSet<(PathBuf, LSPServerType)>,
    /// 每个仓库根目录最后一次有打开文件的时间。
    last_used: HashMap<PathBuf, Instant>,
    scan: Option<AbortHandle>,
}

impl LspLifecycle {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        ctx.subscribe_to_model(&CodeSettings::handle(ctx), |me, event, ctx| {
            if matches!(event, CodeSettingsChangedEvent::EnabledLspServers { .. }) {
                me.remove_disabled_servers(ctx);
            }
        });
        Self::default()
    }

    pub fn status(&self, server: LSPServerType) -> ServerStatus {
        self.statuses.get(&server).cloned().unwrap_or_default()
    }

    /// 正在运行（启动中或可用）这种服务器的仓库数。
    pub fn running_count(&self, server: LSPServerType, ctx: &AppContext) -> usize {
        let manager = LspManagerModel::as_ref(ctx);
        manager
            .workspace_roots()
            .filter(|root| {
                manager
                    .servers_for_workspace(root)
                    .into_iter()
                    .flatten()
                    .any(|model| {
                        let model = model.as_ref(ctx);
                        model.server_type() == server && is_running(model.state())
                    })
            })
            .count()
    }

    /// 检查各服务器是否已安装（PATH 或 Zap 的安装目录）。只检查，不下载。
    pub fn check_installed(&mut self, ctx: &mut ModelContext<Self>) {
        if self.checking_install {
            return;
        }
        self.checking_install = true;
        let http_client = self.http_client(ctx);
        let path_future = LocalShellState::handle(ctx).update(ctx, |shell_state, ctx| {
            shell_state.get_interactive_path_env_var(ctx)
        });
        ctx.spawn(
            async move {
                let executor = CommandBuilder::new(path_future.await);
                let mut results = Vec::new();
                for server in LSPServerType::all() {
                    let installed = server
                        .candidate(http_client.clone())
                        .is_installed(&executor)
                        .await;
                    results.push((server, installed));
                }
                results
            },
            |me, results, ctx| {
                me.checking_install = false;
                for (server, installed) in results {
                    me.statuses.entry(server).or_default().installed = Some(installed);
                }
                ctx.emit(LspLifecycleEvent::StatusChanged);
            },
        );
    }

    fn http_client(&mut self, _ctx: &AppContext) -> Arc<http_client::Client> {
        self.http_client
            .get_or_insert_with(|| Arc::new(http_client::Client::new()))
            .clone()
    }

    /// 在设置里关闭的服务器立即停止并移除，打开文件的诊断随之清除。
    fn remove_disabled_servers(&mut self, ctx: &mut ModelContext<Self>) {
        let enabled = CodeSettings::as_ref(ctx)
            .enabled_lsp_servers
            .value()
            .clone();
        let manager = LspManagerModel::handle(ctx);
        let to_remove: Vec<(PathBuf, LSPServerType)> = manager
            .as_ref(ctx)
            .workspace_roots()
            .flat_map(|root| {
                manager
                    .as_ref(ctx)
                    .servers_for_workspace(root)
                    .into_iter()
                    .flatten()
                    .map(|model| model.as_ref(ctx).server_type())
                    .filter(|server| !enabled.contains(server))
                    .map(|server| (root.clone(), server))
                    .collect::<Vec<_>>()
            })
            .collect();
        if to_remove.is_empty() {
            return;
        }
        manager.update(ctx, |manager, ctx| {
            for (root, server) in to_remove {
                log::info!(
                    "Removing disabled {} LSP server for {}",
                    server.binary_name(),
                    root.display()
                );
                manager.remove_server(&root, server, ctx);
            }
        });
        ctx.emit(LspLifecycleEvent::StatusChanged);
    }

    /// 移除之前启动失败的服务器，让它们重新启动（比如用户刚装好服务器）。
    fn remove_failed_servers(
        &mut self,
        root: &Path,
        servers: &[LSPServerType],
        ctx: &mut ModelContext<Self>,
    ) {
        let manager = LspManagerModel::handle(ctx);
        let failed: Vec<LSPServerType> = manager
            .as_ref(ctx)
            .servers_for_workspace(root)
            .into_iter()
            .flatten()
            .map(|model| model.as_ref(ctx))
            .filter(|model| matches!(model.state(), LspState::Failed { .. }))
            .map(|model| model.server_type())
            .filter(|server| servers.contains(server))
            .collect();
        if failed.is_empty() {
            return;
        }
        manager.update(ctx, |manager, ctx| {
            for server in failed {
                manager.remove_server(root, server, ctx);
            }
        });
    }

    /// 订阅服务器事件：记录启动错误并弹出提示，状态变化时通知设置页。
    fn watch_server(&mut self, server: &ModelHandle<LspServerModel>, ctx: &mut ModelContext<Self>) {
        if !self.watched.insert(server.as_ref(ctx).id()) {
            return;
        }
        let server_type = server.as_ref(ctx).server_type();
        ctx.subscribe_to_model(server, move |me, event, ctx| match event {
            LspEvent::Started => {
                let status = me.statuses.entry(server_type).or_default();
                status.installed = Some(true);
                status.last_error = None;
                ctx.emit(LspLifecycleEvent::StatusChanged);
            }
            LspEvent::Stopped => ctx.emit(LspLifecycleEvent::StatusChanged),
            LspEvent::Failed(e) => {
                let error = format!("{e:#}");
                let status = me.statuses.entry(server_type).or_default();
                // 没装时显示安装命令，而不是一段报错。
                let not_installed = error.contains("is not installed");
                if not_installed {
                    status.installed = Some(false);
                    status.last_error = None;
                } else {
                    status.last_error = Some(error.clone());
                }
                ctx.emit(LspLifecycleEvent::StatusChanged);
                show_failure_toast(server_type, &error, not_installed, ctx);
            }
            _ => {}
        });
    }

    fn ensure_started(
        &mut self,
        path: &Path,
        servers: Vec<LSPServerType>,
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(root) = repo_root_for_path(path, ctx) else {
            log::debug!(
                "Skipping LSP: {} is not in a git repository",
                path.display()
            );
            return;
        };
        self.last_used.insert(root.clone(), Instant::now());
        self.remove_failed_servers(&root, &servers, ctx);

        let app: &AppContext = ctx;
        let manager = LspManagerModel::as_ref(app);
        let to_start: Vec<LSPServerType> = servers
            .into_iter()
            .filter(|server| !manager.server_registered_and_started(&root, *server, app))
            .filter(|server| !self.pending.contains(&(root.clone(), *server)))
            .collect();
        if to_start.is_empty() {
            return;
        }
        for server in &to_start {
            self.pending.insert((root.clone(), *server));
        }

        // gopls 等常装在 ~/go/bin 这类只在交互式 shell 中可见的位置。
        let path_future = LocalShellState::handle(ctx).update(ctx, |shell_state, ctx| {
            shell_state.get_interactive_path_env_var(ctx)
        });
        ctx.spawn(path_future, move |me, path_env_var, ctx| {
            me.start_servers(root, to_start, path_env_var, ctx);
        });
    }

    fn start_servers(
        &mut self,
        root: PathBuf,
        servers: Vec<LSPServerType>,
        path_env_var: Option<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        for server in &servers {
            self.pending.remove(&(root.clone(), *server));
        }

        let http_client = self.http_client(ctx);
        let client_name = ChannelState::app_id().application_name().to_string();
        let configs: Vec<LspServerConfig> = servers
            .into_iter()
            .map(|server| {
                log::info!(
                    "Starting {} LSP server for {}",
                    server.binary_name(),
                    root.display()
                );
                LspServerConfig::new(
                    server,
                    root.clone(),
                    path_env_var.clone(),
                    client_name.clone(),
                    http_client.clone(),
                )
                .with_log_relative_path(relative_log_path(server, &root))
            })
            .collect();

        LspManagerModel::handle(ctx).update(ctx, |manager, ctx| {
            for config in configs {
                // 空闲回收后的服务器仍处于注册状态，这里返回 false，直接复用。
                manager.register(root.clone(), config, ctx);
            }
            manager.start_all(root.clone(), ctx);
        });

        let servers = LspManagerModel::as_ref(ctx)
            .servers_for_workspace(&root)
            .cloned()
            .unwrap_or_default();
        for server in &servers {
            self.watch_server(server, ctx);
        }
        ctx.emit(LspLifecycleEvent::StatusChanged);

        if self.scan.is_none() {
            self.schedule_scan(ctx);
        }
    }

    fn schedule_scan(&mut self, ctx: &mut ModelContext<Self>) {
        let timer = async {
            Timer::after(SCAN_INTERVAL).await;
        };
        let handle = ctx.spawn(timer, |me, _, ctx| {
            me.scan = None;
            if me.stop_idle_servers(ctx) {
                me.schedule_scan(ctx);
            }
        });
        self.scan = Some(handle.abort_handle());
    }

    /// 停止空闲仓库的服务器。返回是否仍有服务器在运行，需要继续检查。
    fn stop_idle_servers(&mut self, ctx: &mut ModelContext<Self>) -> bool {
        let running_roots = running_roots(ctx);
        self.last_used
            .retain(|root, _| running_roots.contains(root));
        if running_roots.is_empty() {
            return false;
        }

        let open_files = open_local_files(ctx);
        let now = Instant::now();
        let mut idle_roots = Vec::new();
        for root in running_roots {
            if open_files.iter().any(|file| file.starts_with(&root)) {
                self.last_used.insert(root, now);
                continue;
            }
            let last_used = *self.last_used.entry(root.clone()).or_insert(now);
            if now.duration_since(last_used) >= IDLE_GRACE_PERIOD {
                idle_roots.push(root);
            }
        }

        for root in &idle_roots {
            self.last_used.remove(root);
        }
        LspManagerModel::handle(ctx).update(ctx, |manager, ctx| {
            for root in idle_roots {
                log::info!("Stopping idle LSP servers for {}", root.display());
                manager.stop_all(root, ctx);
            }
        });
        ctx.emit(LspLifecycleEvent::StatusChanged);
        true
    }
}

impl Entity for LspLifecycle {
    type Event = LspLifecycleEvent;
}

fn is_running(state: &LspState) -> bool {
    matches!(state, LspState::Starting | LspState::Available { .. })
}

fn show_failure_toast(
    server: LSPServerType,
    error: &str,
    not_installed: bool,
    ctx: &mut ModelContext<LspLifecycle>,
) {
    let Some(window_id) = WindowManager::as_ref(ctx).active_window() else {
        return;
    };
    let message = if not_installed {
        crate::t!(
            "lsp-server-not-installed",
            server = server.binary_name(),
            command = install_command(server)
        )
    } else {
        crate::t!(
            "lsp-server-start-failed",
            server = server.binary_name(),
            error = error
        )
    };
    ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
        toast_stack.add_ephemeral_toast(DismissibleToast::error(message), window_id, ctx);
    });
}

impl SingletonEntity for LspLifecycle {}

/// 推荐的安装命令。只作提示，Zap 不会自动执行。
pub fn install_command(server: LSPServerType) -> &'static str {
    match server {
        LSPServerType::Pyright => "npm install -g pyright",
        LSPServerType::TypeScriptLanguageServer => {
            "npm install -g typescript typescript-language-server"
        }
        LSPServerType::GoPls => "go install golang.org/x/tools/gopls@latest",
        LSPServerType::RustAnalyzer => "rustup component add rust-analyzer",
        LSPServerType::Clangd => "brew install llvm",
    }
}

/// 优先用已检测到的仓库，否则向上查找 `.git`。
fn repo_root_for_path(path: &Path, ctx: &AppContext) -> Option<PathBuf> {
    if let Some(root) = DetectedRepositories::as_ref(ctx).get_root_for_path(path) {
        return Some(root);
    }
    let path = path.canonicalize().ok()?;
    path.ancestors()
        .skip(1)
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// 有服务器处于启动中或可用状态的仓库根目录。
fn running_roots(ctx: &AppContext) -> Vec<PathBuf> {
    let manager = LspManagerModel::as_ref(ctx);
    manager
        .workspace_roots()
        .filter(|root| {
            manager
                .servers_for_workspace(root)
                .into_iter()
                .flatten()
                .any(|server| is_running(server.as_ref(ctx).state()))
        })
        .cloned()
        .collect()
}

/// 所有窗口中打开的本地文件。仓库根目录已规范化，这里也规范化后再比较。
fn open_local_files(ctx: &AppContext) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for window_id in ctx.window_ids() {
        let Some(editors) = ctx.views_of_type::<LocalCodeEditorView>(window_id) else {
            continue;
        };
        for editor in editors {
            if let Some(path) = editor.as_ref(ctx).file_path() {
                files.push(path.canonicalize().unwrap_or_else(|_| path.to_path_buf()));
            }
        }
    }
    files
}

/// 服务器 stderr 日志在 LSP 日志目录下的相对路径，例如 `pyright/0123456789abcdef.log`。
/// 仓库路径做哈希，避免过长路径或特殊字符。
fn relative_log_path(server: LSPServerType, root: &Path) -> PathBuf {
    let digest = Sha256::digest(root.to_string_lossy().as_bytes());
    PathBuf::from(server.binary_name()).join(format!("{}.log", hex::encode(&digest[..8])))
}

#[cfg(test)]
#[path = "lsp_lifecycle_tests.rs"]
mod tests;
