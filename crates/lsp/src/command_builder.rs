#[cfg(not(target_arch = "wasm32"))]
use command::r#async::Command;

/// A wrapper around `path_env_var` that produces correctly-configured commands.
///
/// This follows the same wrapping pattern as `command::r#async::Command`:
/// callers construct commands through the executor, which transparently sets
/// the PATH environment variable. On wasm, a dummy implementation is provided
/// so that consumer code doesn't need cfg gating.
#[derive(Clone)]
pub struct CommandBuilder {
    path_env_var: Option<String>,
}

impl CommandBuilder {
    /// Creates a new CommandBuilder with the given PATH environment variable.
    pub fn new(path_env_var: Option<String>) -> Self {
        Self { path_env_var }
    }

    /// Returns the PATH environment variable, if set.
    pub fn path_env_var(&self) -> Option<&str> {
        self.path_env_var.as_deref()
    }

    /// 在 PATH（未设置时用当前进程的 PATH）中查找可执行文件，不运行它。
    ///
    /// 用于不支持 `--version` 之类参数的服务器（如 `pyright-langserver`），
    /// 这类程序一运行就会因缺少 `--stdio` 等参数而报错退出。
    #[cfg(not(target_arch = "wasm32"))]
    pub fn find_on_path(&self, program: &str) -> Option<std::path::PathBuf> {
        let path = match &self.path_env_var {
            Some(path) => std::ffi::OsString::from(path),
            None => std::env::var_os("PATH")?,
        };
        std::env::split_paths(&path)
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    }

    /// Creates a new Command with PATH already set.
    ///
    /// Use this when you need to run a command. The returned Command has the
    /// same API as `command::r#async::Command`, so callers don't need to change
    /// how they construct commands.
    ///
    /// On Windows, the command is wrapped in `cmd.exe /c` so that `.cmd` and
    /// `.bat` scripts on PATH are resolved correctly (e.g. `npm.cmd`,
    /// `typescript-language-server.cmd`). Rust's `Command::new` uses
    /// `CreateProcessW` which only resolves `.exe` extensions.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        #[cfg(windows)]
        let mut cmd = {
            let mut cmd = Command::new("cmd.exe");
            cmd.arg("/c").arg(program);
            cmd
        };
        #[cfg(not(windows))]
        let mut cmd = Command::new(program);
        if let Some(path) = &self.path_env_var {
            cmd.env("PATH", path);
        }
        cmd
    }
}

#[cfg(all(not(target_arch = "wasm32"), unix))]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(all(not(target_arch = "wasm32"), windows))]
fn is_executable(path: &std::path::Path) -> bool {
    ["", "exe", "cmd", "bat"].iter().any(|extension| {
        let candidate = if extension.is_empty() {
            path.to_path_buf()
        } else {
            path.with_extension(extension)
        };
        candidate.is_file()
    })
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::CommandBuilder;

    #[test]
    fn find_on_path_only_matches_executable_files() {
        let dir = std::env::temp_dir().join(format!("zap-lsp-find-on-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let tool = dir.join("fake-langserver");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        let not_executable = dir.join("not-executable");
        std::fs::write(&not_executable, "").unwrap();

        let builder = CommandBuilder::new(Some(format!("/nonexistent:{}", dir.display())));
        assert_eq!(builder.find_on_path("fake-langserver"), None);

        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(builder.find_on_path("fake-langserver"), Some(tool));
        assert_eq!(builder.find_on_path("not-executable"), None);
        assert_eq!(builder.find_on_path("missing"), None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
