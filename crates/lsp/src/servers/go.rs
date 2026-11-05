use std::path::Path;
#[cfg(feature = "local_fs")]
use std::path::PathBuf;
use std::sync::Arc;

use crate::language_server_candidate::{LanguageServerCandidate, LanguageServerMetadata};
#[cfg(feature = "local_fs")]
use crate::supported_servers::CustomBinaryConfig;
use crate::CommandBuilder;
use async_trait::async_trait;

#[cfg(feature = "local_fs")]
const SERVER_NAME: &str = "gopls";

#[cfg(feature = "local_fs")]
const BINARY_NAME: &str = if cfg!(windows) { "gopls.exe" } else { "gopls" };

#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub struct GoPlsCandidate {
    client: Arc<http_client::Client>,
}

impl GoPlsCandidate {
    pub fn new(client: Arc<http_client::Client>) -> Self {
        Self { client }
    }

    /// 依次查找自管安装目录和 `go install` 的默认安装目录（`GOBIN`，否则 `GOPATH/bin`）。
    ///
    /// 用户按提示执行 `go install golang.org/x/tools/gopls@latest` 后，gopls 默认装在
    /// `~/go/bin`，这个目录通常不在 PATH 里。
    ///
    /// # Arguments
    /// * `path_env_var` - 运行 `go env` 时使用的 PATH。
    #[cfg(feature = "local_fs")]
    pub async fn find_installed_binary_config(
        path_env_var: Option<&str>,
    ) -> Option<CustomBinaryConfig> {
        let managed = warp_core::paths::data_dir()
            .join(SERVER_NAME)
            .join(BINARY_NAME);
        if let Some(config) = Self::verify_binary(managed).await {
            return Some(config);
        }

        for binary_path in Self::go_install_binary_paths(path_env_var).await {
            if let Some(config) = Self::verify_binary(binary_path).await {
                return Some(config);
            }
        }
        None
    }

    /// `go install` 会把 gopls 放到的位置：设置了 `GOBIN` 时只有它，否则是每个 `GOPATH` 下的 `bin`。
    #[cfg(feature = "local_fs")]
    async fn go_install_binary_paths(path_env_var: Option<&str>) -> Vec<PathBuf> {
        let output = match CommandBuilder::new(path_env_var.map(str::to_string))
            .command("go")
            .args(["env", "GOBIN", "GOPATH"])
            .output()
            .await
        {
            Ok(output) if output.status.success() => output,
            _ => return vec![],
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut lines = stdout.lines().map(str::trim);
        let gobin = lines.next().unwrap_or_default();
        let gopath = lines.next().unwrap_or_default();

        if !gobin.is_empty() {
            return vec![Path::new(gobin).join(BINARY_NAME)];
        }
        std::env::split_paths(gopath)
            .filter(|dir| !dir.as_os_str().is_empty())
            .map(|dir| dir.join("bin").join(BINARY_NAME))
            .collect()
    }

    #[cfg(feature = "local_fs")]
    async fn verify_binary(binary_path: PathBuf) -> Option<CustomBinaryConfig> {
        if !binary_path.is_file() {
            return None;
        }

        let output = command::r#async::Command::new(&binary_path)
            .arg("version")
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }

        Some(CustomBinaryConfig {
            binary_path,
            prepend_args: vec![],
        })
    }
}

#[async_trait]
#[cfg(feature = "local_fs")]
impl LanguageServerCandidate for GoPlsCandidate {
    async fn should_suggest_for_repo(&self, path: &Path, executor: &CommandBuilder) -> bool {
        if !path.join("go.mod").exists() {
            return false;
        }

        // Check if Go is installed
        executor
            .command("go")
            .arg("version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn is_installed_in_data_dir(&self, executor: &CommandBuilder) -> bool {
        Self::find_installed_binary_config(executor.path_env_var())
            .await
            .is_some()
    }

    async fn is_installed_on_path(&self, executor: &CommandBuilder) -> bool {
        executor
            .command("gopls")
            .arg("version")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    async fn install(
        &self,
        _metadata: LanguageServerMetadata,
        executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        let install_dir = warp_core::paths::data_dir().join(SERVER_NAME);
        async_fs::create_dir_all(&install_dir).await?;

        let output = executor
            .command("go")
            .args(["install", "golang.org/x/tools/gopls@latest"])
            .env("GOBIN", &install_dir)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to install gopls: {}", stderr);
        }

        Ok(())
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        // gopls 通过 `go install ...@latest` 安装，不需要预先访问 GitHub 获取 metadata。
        // 这样与 opencode 一致，也避免 GitHub API rate limit 阻断安装链路。
        let _ = &self.client;
        Ok(LanguageServerMetadata {
            version: "latest".to_string(),
            url: None,
            digest: None,
        })
    }
}

#[async_trait]
#[cfg(not(feature = "local_fs"))]
impl LanguageServerCandidate for GoPlsCandidate {
    async fn should_suggest_for_repo(&self, _path: &Path, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn is_installed_in_data_dir(&self, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn is_installed_on_path(&self, _executor: &CommandBuilder) -> bool {
        false
    }

    async fn install(
        &self,
        _metadata: LanguageServerMetadata,
        _executor: &CommandBuilder,
    ) -> anyhow::Result<()> {
        todo!()
    }

    async fn fetch_latest_server_metadata(&self) -> anyhow::Result<LanguageServerMetadata> {
        todo!()
    }
}
