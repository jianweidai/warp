//! LSP 服务器的按需启动和空闲回收。
//!
//! - 打开本地文件后，只有该语言的服务器在 `code.lsp.enabled_servers` 中才启动。
//!   默认集合为空，所以默认不会启动任何进程。
//! - 每个 git 仓库根目录一组服务器。不在 git 仓库内的文件不启动。
//! - 仓库内已没有打开的文件，并且超过 [`IDLE_GRACE_PERIOD`]，就停止该仓库的服务器。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ::settings::Setting;
use futures::stream::AbortHandle;
use lsp::supported_servers::LSPServerType;
use lsp::{LanguageId, LspManagerModel, LspServerConfig, LspState};
use repo_metadata::repositories::DetectedRepositories;
use sha2::{Digest, Sha256};
use warpui::r#async::Timer;
use warpui::{AppContext, Entity, ModelContext, SingletonEntity};

use crate::channel::ChannelState;
use crate::code::local_code_editor::LocalCodeEditorView;
use crate::settings::CodeSettings;
use crate::terminal::local_shell::LocalShellState;

/// 仓库内没有打开的文件后，再等多久停止服务器。
const IDLE_GRACE_PERIOD: Duration = Duration::from_secs(5 * 60);
/// 有服务器运行时，检查空闲的间隔。没有服务器运行时不检查。
const SCAN_INTERVAL: Duration = Duration::from_secs(30);

/// 本地文件加载完成后调用。没有启用对应语言的服务器时直接返回，不做任何事。
pub fn on_local_file_loaded(path: &Path, ctx: &mut AppContext) {
    let servers = enabled_servers_for_path(path, ctx);
    if servers.is_empty() {
        return;
    }
    LspLifecycle::handle(ctx).update(ctx, |lifecycle, ctx| {
        lifecycle.ensure_started(path, servers, ctx);
    });
}

fn enabled_servers_for_path(path: &Path, ctx: &AppContext) -> Vec<LSPServerType> {
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

#[derive(Default)]
pub struct LspLifecycle {
    /// 只在需要启动服务器时才创建，用于检查安装情况。
    http_client: Option<Arc<http_client::Client>>,
    /// 正在获取 PATH、尚未注册的服务器，避免重复启动。
    pending: HashSet<(PathBuf, LSPServerType)>,
    /// 每个仓库根目录最后一次有打开文件的时间。
    last_used: HashMap<PathBuf, Instant>,
    scan: Option<AbortHandle>,
}

impl LspLifecycle {
    pub fn new() -> Self {
        Self::default()
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

        let http_client = self
            .http_client
            .get_or_insert_with(|| Arc::new(http_client::Client::new()))
            .clone();
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
            manager.start_all(root, ctx);
        });

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
        true
    }
}

impl Entity for LspLifecycle {
    type Event = ();
}

impl SingletonEntity for LspLifecycle {}

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
                .any(|server| {
                    matches!(
                        server.as_ref(ctx).state(),
                        LspState::Starting | LspState::Available { .. }
                    )
                })
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
