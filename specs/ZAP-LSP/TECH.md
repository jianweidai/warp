# Zap 代码查看器：LSP 实现计划

## Context

目标是把 Zap 的文件查看器做成小型代码查看编辑器，补上错误提示、代码跳转和符号搜索，同时控制内存占用。

LSP 曾完整存在，后来被 `efcaa42b`（2026-05-10）删除，之后又过了 476 个提交。所以这次是**有选择地恢复再改造**，不能直接 revert，也不用从零写。

已定决策：
- 第一批打通并实测 Python（pyright）、TypeScript、Go（gopls）。
- Rust、C/C++ 随代码恢复，默认关闭，稍后再调优。
- 代码补全放到后期。
- 不恢复 `PersistedWorkspace` 和已删除的数据库表，启用状态只存全局设置。
- 第一版只支持本地文件，SSH 远程文件不接 LSP。

## 功能清单

| 功能 | 阶段 | 来源 |
|---|---|---|
| 文件默认显示在左边（已改，待 CI 验证） | 0 | 已完成代码 |
| LSP 核心、按需启动、空闲回收 | 1–2 | 恢复后改造 |
| 语法错误检查 | 3 | 恢复 |
| 悬停提示、跳转定义、查找引用 | 4 | 恢复 |
| 格式化 | 5 | 恢复 |
| 设置页（开关、状态、安装） | 6 | 精简重写 |
| 符号搜索（`workspace/symbol`） | 8 | 新写 |
| 代码补全 | 8 | 新写 |
| 重命名、code action | 8 | 新写 |
| Rust、C/C++ 调优 | 8 | 恢复后调优 |
| SSH 远程文件接 LSP | 8 | 新写 |
| Code Review 面板接 LSP | 8 | 恢复 |

文件名搜索已有（`app/src/search/files/model.rs`），不用做。已下线的 codebase index 也不恢复。

## 阶段 0：先把验证路径打通（必须最先做）

**风险：** 向 `release` 分支 push 会触发 `.github/workflows/zap_release.yml`。macOS 构建成功后，它会创建一个正式 GitHub Release，并标记为 latest（`prerelease: false`、`make_latest: "true"`）。`workflow_dispatch` 也走同一流程。因此**不能用它做日常验证**。

该文件注释提到的 `pr-check.yml` 在仓库里也不存在。

1. 从 `release` 切出 `feat/lsp` 分支，把未提交的“文件显示在左边”改动带过去。
2. 新增 `.github/workflows/pr-check.yml`：
   - 只在 `feat/**` 分支和 PR 上触发。
   - 运行 `cargo check` 和相关模块的测试。
   - 不创建 Release。
3. 增加可手动触发的 macOS arm64 打包任务，只上传 workflow artifact，用于下载安装后手动验收。
4. 为“打开文件默认在左侧”补一个测试（`app/src/workspace/view_test.rs`），并让 CI 验证通过。
5. 把本计划复制到 `specs/ZAP-LSP/TECH.md`，再写一条项目 memory 指向它，避免功能遗忘。

**约束：** 默认不在本地编译。任何 push 都先征得确认。

## 阶段 1：恢复 LSP 核心 crate

- 从 `efcaa42b~1` 恢复 `crates/lsp/`（约 170 KB）。
- 在 `Cargo.toml` 和 `app/Cargo.toml` 中恢复：
  - `lsp-types = "0.97.0"`
  - `lsp.workspace = true`
  - feature `"lsp/local_fs"`
- 复用现存的 `crates/jsonrpc`、`crates/node_runtime`。
- 修正客户端名称等残留的 Warp 品牌文案。
- 修复依赖漂移造成的编译错误。

**完成标准：** CI 编译通过，`crates/lsp` 自带测试通过。这一阶段用户看不到变化，也不启动任何进程。

## 阶段 2：启动和回收（内存控制的核心）

新增 `app/src/code/lsp_lifecycle.rs`，替代旧 `PersistedWorkspace` 中的 LSP 逻辑（旧代码不恢复）。

- **设置：** 在 `app/src/settings/code.rs` 恢复 `enabled_lsp_servers`（`code.lsp.enabled_servers`），默认空，并改为 `SyncToCloud::Never`，因为安装情况因机器而异。
- **懒启动：** 打开受支持语言的文件，且该语言已启用时才启动。只开终端不启动。
- **项目根目录：** 用 `DetectedRepositories::get_root_for_path`（`crates/repo_metadata/src/repositories.rs:165`）。不在 git 仓库内的文件第一版不启动。
- **空闲回收：** 恢复 `language_server_shutdown_manager.rs`，并收紧条件：去掉“有终端在该目录就保留”，改为没有打开的相关文件，且超过宽限期（默认 5 分钟）就停止。
- **退出清理：** 在 `app/src/lib.rs` 恢复 `LspManagerModel::terminate`。

恢复的旧代码：`lsp_logs.rs`、`lsp_telemetry.rs`（遥测保持默认关闭）。

**实际实现（2026-11-04）：**
- 启动、空闲回收和日志路径都放在 `lsp_lifecycle.rs` 一个文件里，没有单独恢复 `language_server_shutdown_manager.rs` 和 `lsp_logs.rs`。
- 遥测不恢复（`lsp_telemetry.rs`）。
- 根目录先查 `DetectedRepositories`，查不到再向上找 `.git`。
- 有服务器运行时每 30 秒检查一次，没有服务器运行时不检查。
- 入口在 `LocalCodeEditorView` 收到 `BufferLoaded` 时，远程文件不会触发。
- 启动失败目前只写日志，提示放到阶段 6 的状态显示里做。

**完成标准：** 启用 pyright 后，打开 `.py` 文件才有子进程；关闭文件超过宽限期后进程退出；退出 Zap 后没有残留进程。

## 阶段 3：文档同步和语法错误检查

- **文档同步：** 在 `app/src/code/global_buffer_model.rs` 手工接回 `didOpen`、`didChange`、`didClose`（当前位置约 `:816`）。这个文件删除后变化最大，不能照搬，需要跳过 `BufferLocation::Remote`。`didChange` 要做防抖。
- **诊断处理：** 恢复 `app/src/code/language_server_extension.rs` 的诊断部分（`refresh_diagnostics`、`ProcessedDiagnostic`）。
- **渲染：** 复用仍然存在的 `set_diagnostic_decorations`（`app/src/code/editor/view.rs:775`）。

**要先处理的问题：** 这个方法和搜索高亮（`app/src/code/editor/view.rs:694`、`:769`）都调用 `set_text_decorations`，可能互相覆盖。需要确认后合并两者。

**完成标准：** 写错代码后约 1 秒内出现下划线，改对后消失，文件内搜索高亮同时正常。鼠标悬停看错误信息移到阶段 4，和悬停提示一起做。

**实际实现（2026-11-04）：**
- 同步逻辑放在 `app/src/code/lsp_document_sync.rs`，作为 `global_buffer_model` 的子模块，原文件只加了几处调用。
- 只同步本地文件。客户端 app 调用 `subscribe_to_lsp_manager` 后才生效，remote-server daemon 不受影响。
- 编辑停止 300 毫秒后发送整份内容，不发增量，避免位置换算出错。
- 诊断显示在 `app/src/code/lsp_diagnostics.rs`，作为 `local_code_editor` 的子模块。只显示错误和警告，且只显示与当前缓冲区版本一致的结果。
- 编辑器分别保存搜索高亮和诊断下划线，渲染前合并，不再互相覆盖。
- 只在文件对应语言已启用 LSP 时才连接诊断。打开文件后再启用，需要重新打开文件。

## 阶段 4：悬停、跳转和引用

- **悬停提示：** 接回 `CodeEditorEvent::MouseHovered`（`app/src/code/local_code_editor.rs:232`，当前为空操作）和 `hover_for_offset`。
- **跳转定义：** 恢复 `call_goto_definition` 和 Cmd+点击。跨文件跳转复用 `open_code`（`app/src/workspace/view.rs`）现有路径。
- **查找引用：** 恢复 `app/src/code/find_references_view.rs`，以及 `app/src/code/editor/element.rs` 的 gutter 位置缓存和 `app/src/code/editor/view/vim_handler.rs` 的相关按键。

**完成标准：** 三种语言都能悬停显示类型、跨文件跳转和列出引用。

**实际实现（2026-11-04）：**
- 悬停和跳转在 `app/src/code/lsp_hover.rs`（`local_code_editor` 的子模块）。鼠标停留 500 毫秒后请求，同时显示该位置的错误信息。
- 悬停内容用 Markdown 文本渲染，不像旧版那样为每个代码块创建一个编辑器，节省内存。
- Cmd+悬停显示链接，点击跳到定义；已经在定义处时，点击改为查找引用。
- 查找引用：Shift+F12，或在定义处 Cmd+点击。结果卡片在 `app/src/code/find_references_view.rs` 重写，每行用纯文本显示，最多 500 条；方向键选择、回车打开、Esc 关闭。
- 跳到项目外的文件（如标准库）时，那个文件暂时没有 LSP 功能。
- 未恢复右键菜单和 Vim 的 `gd`、`gr`、`K` 按键，放到后续。

## 阶段 5：格式化

- 接回 `format_document`，提供手动格式化命令。
- 在 `save_local`（`app/src/code/local_code_editor.rs:622`）增加“保存时格式化”，由新设置控制，**默认关闭**。

**完成标准：** 手动格式化正确；默认保存不改动内容，开启设置后才格式化；服务器未就绪或超时不影响保存。

**实际实现（2026-11-04）：**
- 在 `app/src/code/lsp_format.rs`（`local_code_editor` 的子模块）。手动格式化快捷键 Shift+Alt+F。
- 设置 `code.lsp.format_on_save`，默认关闭。
- 请求前先把延迟中的编辑发给服务器并等待完成，避免按旧内容格式化。
- 3 秒超时；超时、失败或服务器未就绪时照常保存。等待期间内容被修改则丢弃结果。

## 阶段 6：设置页和状态显示

- 在 `app/src/settings_view/code_page.rs` 写精简版，不恢复旧版约 1000 行页面：
  - 每种语言的开关和运行状态
  - 启动、重启、查看日志
  - 查看服务器进程内存
- **服务器查找顺序：** 优先用 PATH 中已安装的，其次是 Zap 下载的。下载只在用户确认后进行（`crates/lsp/src/install.rs`），绝不静默下载。
- 文件底部（`app/src/code/footer.rs`）只显示简单状态：启动中、就绪、错误数。不恢复旧版约 1900 行实现。
- 补齐中英日文案（`app/i18n/*/warp.ftl`）。

**完成标准：** 不改配置文件就能完成启用、安装、停用，状态显示准确。

## 阶段 7：内存实测和验收

在真实项目上记录 Zap 主进程和语言服务器的内存，分别测试：未启用、打开文件、索引完成、关闭后回收。

| 验收项 | 标准 |
|---|---|
| LSP 全部关闭 | Zap 内存相比基线（当前约 579 MB）无明显增长，没有子进程 |
| 关闭文件后 | 超过宽限期，服务器进程全部退出，内存回落 |
| 同一项目反复开关文件 | 不产生重复进程 |

未达标就回到阶段 2 调整。结果写入 `specs/ZAP-LSP/TECH.md`。阶段 7 完成后，第一版才算结束。

## 阶段 8：后续功能（不排期，防遗忘）

按建议顺序：
1. 符号搜索：接入命令面板（`app/src/search/command_palette/`）
2. 代码补全：工作量最大，需要新写弹窗和交互
3. 重命名、code action
4. Rust、C/C++ 调优：rust-analyzer 在大项目上占用高，需要单独设内存提示
5. SSH 远程文件接 LSP
6. Code Review 面板接 LSP

## 风险

- **手工合并量大：** `global_buffer_model.rs`、`app/src/code/view.rs`、`local_code_editor.rs` 自删除后共 +1438 行。每阶段单独成提交，便于回退。
- **上游冲突：** 与 `zerx-lab/warp` 差异会变大。新逻辑尽量放在新文件，减少对现有文件的修改。
- **本地不编译：** 每次验证都依赖 CI，反馈较慢。阶段 0 必须先做好。
- **服务器版本差异：** 旧实现的下载链接和安装逻辑可能已过期，阶段 6 要重新确认。

## 验证方式

- **每阶段：** push 到 `feat/lsp`（先确认），由 `pr-check.yml` 编译和跑测试。单元测试放在恢复的 `crates/lsp` 和 `app/src/code/` 下。
- **端到端：** 从手动打包任务下载 macOS 安装包，用 Python、TypeScript、Go 小项目逐项走各阶段的“完成标准”。
- **合并前：** 所有阶段验证通过，且只有用户确认后才合入 `release`，因为这会发布正式 Release。
