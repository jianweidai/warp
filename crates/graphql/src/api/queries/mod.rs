// OpenWarp Wave 3-2:`get_cloud_environments` query 唯一消费方随 Wave 1-1 下线后 0 引用,文件物理删除。
// OpenWarp Wave 2-1:`get_cloud_object` query 唯一消费方
// `ObjectClient::fetch_single_cloud_object` 已本地化为 Err stub,文件物理删除。
//
// OpenWarp Wave 2-2:再删 6 个 AI query —
// `free_available_models` / `get_feature_model_choices` / `get_request_limit_info`
// / `task_attachments` / `get_ai_conversation_format` / `list_ai_conversations`
// — 唯一消费方 `AIClient impl for ServerApi` 已本地 stub Err。
// `get_scheduled_agent_history` 中的 `ScheduledAgentHistory` 类型仍被
// `app/src/ai/agent_sdk/schedule.rs` + `ambient_agents/scheduled.rs` 使用,
// operation 本身不再消费但文件保留。
//
// OpenWarp Wave 3-1:再删 3 个 auth-only query —
// `api_keys` / `get_user` / `get_conversation_usage` — 唯一消费方
// `AuthClient impl for ServerApi` 已随 server_api/auth.rs 整文件物理删,
// AuthManager 本地 stub 不再请求用户/会话用量元数据。
//
// OpenWarp Wave 4-1:再删 2 个 managed-secrets query —
// `list_managed_secrets` / `managed_secret_config` — 唯一消费方
// `ManagedSecretsClient impl for ServerApi` 已 stub 为 `Ok(empty)`。
// `task_secrets` query 文件保留:其内嵌的 `ManagedSecretValue` enum 仍被
// `crates/managed_secrets` 与 `app/src/ai/agent_sdk` 多处作为 BYOP 类型 import。
pub mod get_oauth_connect_tx_status;
pub mod get_scheduled_agent_history;
pub mod get_updated_cloud_objects;
pub mod get_workspaces_metadata_for_user;
pub mod list_warp_dev_images;
pub mod task_secrets;
