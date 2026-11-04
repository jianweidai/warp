//! Code 设置页的语言服务器部分：每种语言一个开关，开关下方显示安装和运行状态。
//!
//! 只检查是否已安装，不自动下载。未安装时显示安装命令，由用户自己安装。

use ::settings::Setting as _;
use lsp::supported_servers::LSPServerType;
use warpui::{
    elements::Element, ui_components::components::UiComponent,
    ui_components::switch::SwitchStateHandle, AppContext, SingletonEntity,
};

use super::{CodeSettingsPageAction, CodeSettingsPageView};
use crate::appearance::Appearance;
use crate::code::lsp_lifecycle::LspLifecycle;
use crate::settings::CodeSettings;
use crate::settings_view::{
    settings_page::{render_body_item, SettingsWidget},
    LocalOnlyIconState, ToggleState,
};

/// 设置页显示的服务器，首批支持的语言排在前面。
const SERVERS: [LSPServerType; 5] = [
    LSPServerType::Pyright,
    LSPServerType::TypeScriptLanguageServer,
    LSPServerType::GoPls,
    LSPServerType::RustAnalyzer,
    LSPServerType::Clangd,
];

pub(super) fn widgets() -> Vec<Box<dyn SettingsWidget<View = CodeSettingsPageView>>> {
    SERVERS
        .into_iter()
        .map(|server| {
            Box::new(LspServerToggleWidget {
                server,
                switch_state: Default::default(),
            }) as Box<dyn SettingsWidget<View = CodeSettingsPageView>>
        })
        .collect()
}

struct LspServerToggleWidget {
    server: LSPServerType,
    switch_state: SwitchStateHandle,
}

impl SettingsWidget for LspServerToggleWidget {
    type View = CodeSettingsPageView;

    fn search_terms(&self) -> &str {
        match self.server {
            LSPServerType::Pyright => {
                "lsp language server diagnostics hover definition python pyright"
            }
            LSPServerType::TypeScriptLanguageServer => {
                "lsp language server diagnostics hover definition typescript javascript"
            }
            LSPServerType::GoPls => "lsp language server diagnostics hover definition go gopls",
            LSPServerType::RustAnalyzer => {
                "lsp language server diagnostics hover definition rust rust-analyzer"
            }
            LSPServerType::Clangd => {
                "lsp language server diagnostics hover definition c c++ clangd"
            }
        }
    }

    fn render(
        &self,
        _view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let server = self.server;
        let enabled = CodeSettings::as_ref(app)
            .enabled_lsp_servers
            .value()
            .contains(&server);

        render_body_item::<CodeSettingsPageAction>(
            crate::t!(
                "settings-code-lsp-server",
                language = server.language_name(),
                binary = server.binary_name()
            ),
            None,
            LocalOnlyIconState::Hidden,
            ToggleState::Enabled,
            appearance,
            appearance
                .ui_builder()
                .switch(self.switch_state.clone())
                .check(enabled)
                .build()
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(CodeSettingsPageAction::ToggleLspServer(server));
                })
                .finish(),
            Some(status_text(server, enabled, app)),
        )
    }
}

fn status_text(server: LSPServerType, enabled: bool, app: &AppContext) -> String {
    let lifecycle = LspLifecycle::as_ref(app);
    let status = lifecycle.status(server);
    if let Some(error) = status.last_error.filter(|_| enabled) {
        return crate::t!("settings-code-lsp-status-failed", error = error);
    }
    let running = lifecycle.running_count(server, app);
    if enabled && running > 0 {
        return crate::t!("settings-code-lsp-status-running", count = running);
    }
    match status.installed {
        None => crate::t!("settings-code-lsp-status-checking"),
        Some(false) => crate::t!(
            "settings-code-lsp-status-not-installed",
            command = install_command(server)
        ),
        Some(true) if enabled => crate::t!("settings-code-lsp-status-enabled"),
        Some(true) => crate::t!("settings-code-lsp-status-installed"),
    }
}

/// 推荐的安装命令。只作提示，Zap 不会自动执行。
fn install_command(server: LSPServerType) -> &'static str {
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
