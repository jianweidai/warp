// OpenWarp:BlockClient impl 已本地化为 stub。
// 历史职责:通过 GraphQL `share_block` / `unshare_block` / `get_blocks_for_user`
// 与 warp.dev 后端往返,实现"分享 block 到云端"+"在设置→Shared blocks 页面列出/取消分享我已分享的 block"。
// OpenWarp 不需要这条云端腿:
//   - `save_block` 直接返回错误(UI 入口 share_block_modal 已在更早 Phase 删除,但 trait
//     仍被 settings_view/show_blocks_view.rs 引用,故 stub 保留接口)
//   - `unshare_block` no-op 返回 Ok(())
//   - `blocks_owned_by_user` 返回空 Vec,Shared blocks 设置页会渲染"空列表"
//   - `generate_shared_block_title` 0 外部消费点,stub 化直接报错
// `crate::server::block::DisplaySetting` 与 `Block` 数据类型保留(本地遥测枚举 + terminal model
// + 设置页 UI 仍消费 DisplaySetting,Block 仅作为本地传输结构)。
// 相关 GraphQL operation 已在同一 PR 物理删除:
//   crates/graphql/src/api/mutations/{share_block,unshare_block}.rs
//   crates/graphql/src/api/queries/get_blocks_for_user.rs

use super::ServerApi;
use crate::ai::generate_block_title::api::{GenerateBlockTitleRequest, GenerateBlockTitleResponse};
use crate::server::block::{Block, DisplaySetting};
use anyhow::anyhow;
use async_trait::async_trait;
#[cfg(test)]
use mockall::automock;

#[cfg_attr(test, automock)]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait BlockClient: 'static + Send + Sync {
    /// Unshares a block identified at `block_id`.
    async fn unshare_block(&self, block_id: String) -> Result<(), anyhow::Error>;

    /// Uploads a given block to the server via the /share_block endpoint.
    async fn save_block(
        &self,
        block: &Block,
        title: Option<String>,
        show_prompt: bool,
        display_setting: DisplaySetting,
    ) -> Result<String, anyhow::Error>;

    async fn blocks_owned_by_user(&self) -> Result<Vec<Block>, anyhow::Error>;

    async fn generate_shared_block_title(
        &self,
        request: GenerateBlockTitleRequest,
    ) -> Result<GenerateBlockTitleResponse, anyhow::Error>;
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl BlockClient for ServerApi {
    async fn unshare_block(&self, _block_uid: String) -> Result<(), anyhow::Error> {
        // OpenWarp:云端 unshare 已下线,本地 stub 直接返回 Ok。
        Ok(())
    }

    async fn save_block(
        &self,
        _block: &Block,
        _title: Option<String>,
        _show_prompt: bool,
        _display_setting: DisplaySetting,
    ) -> Result<String, anyhow::Error> {
        // OpenWarp:云端 share block 已下线,本地 stub 直接报错,UI 入口已删,理论上不可达。
        Err(anyhow!("Block sharing is disabled in OpenWarp"))
    }

    async fn blocks_owned_by_user(&self) -> Result<Vec<Block>, anyhow::Error> {
        // OpenWarp:云端 list shared blocks 已下线,Shared blocks 设置页面将渲染空列表。
        Ok(Vec::new())
    }

    async fn generate_shared_block_title(
        &self,
        _request: GenerateBlockTitleRequest,
    ) -> Result<GenerateBlockTitleResponse, anyhow::Error> {
        // OpenWarp:云端 /ai/generate_block_title 端点已下线,本地 stub 直接报错。
        // 该方法当前无外部消费点。
        Err(anyhow!(
            "Shared block title generation is disabled in OpenWarp"
        ))
    }
}
