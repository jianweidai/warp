//! `persisted_workspace.rs` 单元测试。
//!
//! 这里只覆盖**纯数据结构层**的不变量(`root_for_workspace`)。
//! `workspace_root_for_lsp` 还会去查 `RepoMetadataModel`,需要完整 AppContext +
//! singleton 装配,放到 integration 测试里更合适。

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ai::workspace::WorkspaceMetadata;
#[cfg(feature = "local_fs")]
use lsp::supported_servers::LSPServerType;

#[cfg(feature = "local_fs")]
use super::merge_enabled_and_auto_start;
use super::{PersistedWorkspace, Workspace};

/// 构造一个空的 `PersistedWorkspace`,等价于 `new_for_test` 但不需要 ModelContext。
fn empty_persisted_workspace() -> PersistedWorkspace {
    PersistedWorkspace {
        workspaces: HashMap::new(),
        model_event_sender: None,
        #[cfg(feature = "local_fs")]
        lsp_installation_status: HashMap::new(),
    }
}

/// 往 `self.workspaces` 里塞一个空的 workspace 条目,模拟"已通过 user_added_workspace
/// 注册过的旧持久化 workspace"。
fn insert_workspace(pw: &mut PersistedWorkspace, path: &Path) {
    pw.workspaces.insert(
        path.to_path_buf(),
        Workspace {
            metadata: WorkspaceMetadata {
                path: path.to_path_buf(),
                navigated_ts: None,
                modified_ts: None,
                queried_ts: None,
            },
            language_servers: HashMap::new(),
        },
    );
}

#[test]
fn root_for_workspace_returns_none_when_unregistered() {
    // 没有任何已注册条目时,`root_for_workspace` 必须返回 None,
    // 让 `workspace_root_for_lsp` 走 RepoMetadata fallback 路径。
    let pw = empty_persisted_workspace();
    let repo = PathBuf::from("/tmp/some-fresh-repo");

    assert!(pw.root_for_workspace(&repo).is_none());
}

#[test]
fn root_for_workspace_resolves_registered_ancestor() {
    // 子目录路径解析到注册过的祖先,这是 LSP 启动时
    // workspace_root 复用旧 persisted workspace 的关键路径。
    let mut pw = empty_persisted_workspace();
    let repo = PathBuf::from("/tmp/registered-repo");
    insert_workspace(&mut pw, &repo);

    let nested = repo.join("src/foo/bar.rs");
    assert_eq!(pw.root_for_workspace(&nested), Some(repo.as_path()));
}

#[test]
fn root_for_workspace_ignores_unrelated_registered_workspace() {
    // `self.workspaces` 里的不相关条目不应该污染 lookup。
    let mut pw = empty_persisted_workspace();
    insert_workspace(&mut pw, &PathBuf::from("/tmp/some-other-repo"));

    let unrelated = PathBuf::from("/tmp/unrelated-repo/src/main.rs");
    assert!(pw.root_for_workspace(&unrelated).is_none());
}

#[cfg(feature = "local_fs")]
#[test]
fn merge_enabled_and_auto_start_preserves_global_order_and_appends_new() {
    // 全局启用集合位于前面,auto-start 中未出现过的追加到后面。
    let merged = merge_enabled_and_auto_start(
        vec![LSPServerType::RustAnalyzer],
        vec![
            LSPServerType::GoPls,
            LSPServerType::TypeScriptLanguageServer,
        ],
    );
    assert_eq!(
        merged,
        vec![
            LSPServerType::RustAnalyzer,
            LSPServerType::GoPls,
            LSPServerType::TypeScriptLanguageServer,
        ]
    );
}

#[cfg(feature = "local_fs")]
#[test]
fn merge_enabled_and_auto_start_dedups_overlap() {
    // auto-start 中与全局重复的部分不重复出现。
    let merged = merge_enabled_and_auto_start(
        vec![LSPServerType::RustAnalyzer, LSPServerType::GoPls],
        vec![
            LSPServerType::GoPls,
            LSPServerType::TypeScriptLanguageServer,
        ],
    );
    assert_eq!(
        merged,
        vec![
            LSPServerType::RustAnalyzer,
            LSPServerType::GoPls,
            LSPServerType::TypeScriptLanguageServer,
        ]
    );
}

#[cfg(feature = "local_fs")]
#[test]
fn merge_enabled_and_auto_start_empty_inputs() {
    // 全为空 → 结果为空。
    let merged: Vec<LSPServerType> = merge_enabled_and_auto_start(vec![], vec![]);
    assert!(merged.is_empty());

    // 只有 auto-start → 返回 auto-start。BYOP 默认场景:
    // 用户什么都没设置,进入 Rust 仓库 → rust-analyzer 被 auto-start。
    let merged = merge_enabled_and_auto_start(vec![], vec![LSPServerType::RustAnalyzer]);
    assert_eq!(merged, vec![LSPServerType::RustAnalyzer]);
}
