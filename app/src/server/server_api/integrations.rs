// OpenWarp:IntegrationsClient impl 已收敛为仅保留 BYOP OAuth flow 所需的 OAuth
// transaction 状态轮询能力。
//
// 历史职责:通过 GraphQL `create_simple_integration` / `get_simple_integrations` /
// `get_integrations_using_environment` / `suggest_cloud_environment_image` /
// `user_github_info` / `user_repo_auth_status` 与 warp.dev 后端往返,服务于:
//   - 云端 Simple Integration(Linear / Slack 等)CRUD
//   - 云端 cloud agent environment 的 Docker image 建议 + GitHub 仓库授权检查
//   - 设置→Environments 页面拉取用户的 GitHub 已连接仓库
// OpenWarp 不需要这些云端腿,本地用户走 BYOP OAuth provider flow 即可,故:
//   - trait 上仅保留 `poll_oauth_connect_status`(BYOP 本地 OAuth flow,
//     消费点 `app/src/ai/agent_sdk/oauth_flow.rs::poll_oauth_until_terminal`)
//   - 其它方法全部从 trait 删除;原 CLI 入口 (`warp agent integration *`)
//     已在同 PR 物理删除 `app/src/ai/agent_sdk/integration*.rs`
// 相关 GraphQL operation 已在同一 PR 物理删除:
//   crates/graphql/src/api/mutations/create_simple_integration.rs
//   crates/graphql/src/api/queries/{get_simple_integrations,
//     get_integrations_using_environment,suggest_cloud_environment_image,
//     user_github_info,user_repo_auth_status}.rs

use super::ServerApi;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use cynic::QueryBuilder;

#[cfg(test)]
use mockall::automock;

use crate::server::graphql::{get_request_context, get_user_facing_error_message};
use warp_graphql::queries::get_oauth_connect_tx_status::{
    GetOAuthConnectTxStatus, GetOAuthConnectTxStatusInput, GetOAuthConnectTxStatusResult,
    GetOAuthConnectTxStatusVariables, OauthConnectTxStatus,
};

#[cfg(not(target_family = "wasm"))]
pub trait IntegrationsClientBounds: Send + Sync {}

#[cfg(not(target_family = "wasm"))]
impl<T: 'static + Send + Sync> IntegrationsClientBounds for T {}

#[cfg(target_family = "wasm")]
pub trait IntegrationsClientBounds {}

#[cfg(target_family = "wasm")]
impl<T: 'static> IntegrationsClientBounds for T {}

#[cfg_attr(test, automock)]
#[cfg_attr(target_family = "wasm", allow(dead_code))]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
pub trait IntegrationsClient: 'static + IntegrationsClientBounds {
    /// Polls the status of an OAuth connect transaction.
    ///
    /// OpenWarp:BYOP 本地 OAuth provider flow 仍然依赖该轮询,
    /// 由 `agent_sdk/oauth_flow.rs::poll_oauth_until_terminal` 消费。
    ///
    /// # Arguments
    /// * `tx_id` - The transaction ID returned from the OAuth start request
    ///
    /// # Returns
    /// * `Ok(OauthConnectTxStatus)` - The current status of the transaction
    /// * `Err` - If the transaction is not found or polling fails
    async fn poll_oauth_connect_status(&self, tx_id: String) -> Result<OauthConnectTxStatus>;
}

#[cfg_attr(target_family = "wasm", async_trait(?Send))]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
impl IntegrationsClient for ServerApi {
    async fn poll_oauth_connect_status(&self, tx_id: String) -> Result<OauthConnectTxStatus> {
        let variables = GetOAuthConnectTxStatusVariables {
            request_context: get_request_context(),
            input: GetOAuthConnectTxStatusInput {
                tx_id: cynic::Id::new(tx_id),
            },
        };

        let operation = GetOAuthConnectTxStatus::build(variables);
        let response = self.send_graphql_request(operation, None).await?;

        match response.get_oauth_connect_tx_status {
            GetOAuthConnectTxStatusResult::GetOAuthConnectTxStatusOutput(output) => {
                Ok(output.status)
            }
            GetOAuthConnectTxStatusResult::UserFacingError(error) => {
                Err(anyhow!(get_user_facing_error_message(error)))
            }
            GetOAuthConnectTxStatusResult::Unknown => {
                Err(anyhow!("Unknown error while polling OAuth status"))
            }
        }
    }
}
