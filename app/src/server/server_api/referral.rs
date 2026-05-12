// OpenWarp(本地化,Phase 5):`ReferralsClient` 是云端推荐邀请 RPC,本地化场景下无 referral
// 概念。trait 定义保留作为测试 mock 与 settings UI 字段类型,impl 改 stub 返回 Err。
// 实际 UI (`settings_view/referrals_page.rs`) 会展示 fetch 失败状态。

use super::ServerApi;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
#[cfg(test)]
use mockall::{automock, predicate::*};

/// Referral information for the logged-in user
pub struct ReferralInfo {
    /// Shareable URL that the user can use to invite friends
    pub url: String,
    /// The underlying referral code associated with the user
    pub code: String,
    /// Number of other users who have signed up with this user's referral code
    pub number_claimed: usize,
    /// Whether the user has been referred by another user
    pub is_referred: bool,
}

#[cfg_attr(test, automock)]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait ReferralsClient: 'static + Send + Sync {
    /// Gets the user's referral information.
    async fn get_referral_info(&self) -> Result<ReferralInfo>;

    /// Send one or more email invites.
    async fn send_invite(&self, emails: Vec<String>) -> Result<Vec<String>>;
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl ReferralsClient for ServerApi {
    async fn get_referral_info(&self) -> Result<ReferralInfo> {
        Err(anyhow!("Referrals disabled in OpenWarp"))
    }

    async fn send_invite(&self, _emails: Vec<String>) -> Result<Vec<String>> {
        Err(anyhow!("Referrals disabled in OpenWarp"))
    }
}
