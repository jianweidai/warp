// OpenWarp Wave 2-1:ObjectClient impl 完全本地化。
//
// 历史背景:
// - trait 接口签名严格保留(Wave 3 再统一删 trait 本身、UpdateManager、SyncQueue 等
//   "云端同步"骨架),只重写 `impl ObjectClient for ServerApi` 的 30 个方法体。
// - UpdateManager 上层在 Phase 2c-2 / 2c-3 已对 `create_object` / `update_object` 做了
//   本地化(`new_local` + 写 sqlite,不再调用 `create_object_online` / `update_object_online`),
//   所以 A 类(创建/更新)在 OpenWarp 默认路径不可达。但 `SyncQueue` 在 `start_dequeueing`
//   被触发时仍会通过 `CloudModelType::send_create_request` 等间接调用 `ObjectClient`
//   方法,所以这一层必须返回合成成功响应,不能 panic 也不能 Err(否则 SyncQueue 会无限重试)。
// - B 类(trash / untrash / delete / empty_trash / move):UpdateManager 中**有 server_id**
//   的对象路径仍调 `object_client.trash_object` 等;返回 `Ok(成功)` 让上层
//   `RequestSucceeded` 分支跑完(清 `has_pending_metadata_change`,写 sqlite)。
//   本地化场景对象大多走 `id.server_id() == None` 短路腿,但带 server_id 的旧对象
//   (从 cloud 同步过来的)仍走这条 RPC 腿,必须返回合成成功值。
// - C 类(剩余 ~10 个):云端 share / guest / owner transfer / action audit /
//   cloud environment timestamps —— 单机无对应概念,全 no-op 或 Err。
//
// 关键设计:
// - 合成 server_id 不能与本地对象的 server_id 冲突。采用 `ServerId::new_random_for_local()`
//   语义 —— 使用 `ServerId::from(u64)` + 时间戳种子,与 FakeObjectClient 的 `alloc_server_id`
//   保持思路一致(但 ServerApi 是无状态的,所以用时间戳)。
// - 合成 `Revision` 用 `Revision::from(Utc::now())`(public,非 test-util gated)。
// - 不再 import 任何 `cynic::*` / `warp_graphql::mutations::*` / `warp_graphql::queries::*`
//   ObjectClient 范围的 operation,清掉 GraphQL 依赖。
// - `UpdatedObjectInput`(`get_updated_cloud_objects` query 的 input 类型)仍被
//   `cloud_object::CloudObject::versions` trait 方法使用,query module 本身保留;
//   本任务只删 21 个 mutation + 0 query(get_cloud_object 唯一消费方在
//   `fetch_single_cloud_object` impl,删之)。

use crate::cloud_object::SerializedModel;
use crate::{
    cloud_object::{
        model::generic_string_model::GenericStringObjectId, BulkCreateCloudObjectResult,
        BulkCreateGenericStringObjectsRequest, CreateCloudObjectResult, CreateObjectRequest,
        CreatedCloudObject, GenericStringObjectFormat, GenericStringObjectUniqueKey,
        ObjectDeleteResult, ObjectIdType, ObjectMetadataUpdateResult, ObjectType, ObjectsToUpdate,
        Owner, Revision, RevisionAndLastEditor, ServerFolder, ServerMetadata, ServerNotebook,
        ServerObject, ServerPermissions, ServerWorkflow, UpdateCloudObjectResult,
    },
    drive::folders::FolderId,
    notebooks::NotebookId,
    server::{
        cloud_objects::update_manager::{GetCloudObjectResponse, InitialLoadResponse},
        ids::{HashableId, ServerId, ServerIdAndType, SyncId},
        server_api::ServerApi,
    },
    workflows::WorkflowId,
};
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
#[cfg(test)]
use mockall::{automock, predicate::*};
use std::collections::HashMap;
use warp_graphql::scalars::time::ServerTimestamp;

#[cfg_attr(test, automock)]
#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
pub trait ObjectClient: 'static + Send + Sync {
    /// This method saves a workflow for a given owner and returns it on success.
    async fn create_workflow(
        &self,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult>;

    /// Updates a workflow with the new data. The update may be rejected if a revision
    /// is specified _and_ that revision is not the current revision of the object in storage.
    async fn update_workflow(
        &self,
        workflow_id: WorkflowId,
        data: SerializedModel,
        revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<ServerWorkflow>>;

    /// Creates n generic string objects in a single graphql request. Use
    /// this rather than calling create_generic_string_object multiple times
    /// in a loop.
    async fn bulk_create_generic_string_objects(
        &self,
        owner: Owner,
        objects: &[BulkCreateGenericStringObjectsRequest],
    ) -> Result<BulkCreateCloudObjectResult>;

    async fn create_generic_string_object(
        &self,
        format: GenericStringObjectFormat,
        uniqueness_key: Option<GenericStringObjectUniqueKey>,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult>;

    /// Creates a notebook on the server, returning the ID and revision of the object after
    /// creation.
    async fn create_notebook(
        &self,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult>;

    /// Updates a notebook with the new title and data. The update may be rejected if a revision
    /// is specified _and_ that revision is not the current revision of the object in storage.
    async fn update_notebook(
        &self,
        notebook_id: NotebookId,
        title: Option<String>,
        data: Option<SerializedModel>,
        revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<ServerNotebook>>;

    async fn create_folder(&self, request: CreateObjectRequest) -> Result<CreateCloudObjectResult>;

    async fn update_folder(
        &self,
        folder_id: FolderId,
        name: SerializedModel,
    ) -> Result<UpdateCloudObjectResult<ServerFolder>>;

    async fn update_generic_string_object(
        &self,
        object_id: GenericStringObjectId,
        model: SerializedModel,
        revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<Box<dyn ServerObject>>>;

    /// Sets the current editor of the notebook to be the logged in user
    async fn grab_notebook_edit_access(&self, notebook_id: NotebookId) -> Result<ServerMetadata>;
    /// Sets the current editor of the notebook to be null
    async fn give_up_notebook_edit_access(&self, notebook_id: NotebookId)
        -> Result<ServerMetadata>;

    // OpenWarp(本地化,Phase 2d-4a-1):原 `get_warp_drive_updates` GraphQL Subscription
    // 在 listener.rs 物理删除后无调用方,从 trait 中移除。

    async fn fetch_changed_objects(
        &self,
        objects_to_update: ObjectsToUpdate,
        force_refresh: bool,
    ) -> Result<InitialLoadResponse>;

    async fn fetch_single_cloud_object(&self, id: ServerId) -> Result<GetCloudObjectResponse>;

    // Transfers a notebook to the given owner
    async fn transfer_notebook_owner(&self, notebook_id: NotebookId, owner: Owner) -> Result<bool>;

    async fn transfer_workflow_owner(&self, workflow_id: WorkflowId, owner: Owner) -> Result<bool>;

    async fn transfer_generic_string_object_owner(
        &self,
        workflow_id: GenericStringObjectId,
        owner: Owner,
    ) -> Result<bool>;

    async fn trash_object(&self, id: ServerId) -> Result<bool>;

    async fn untrash_object(&self, id: ServerId) -> Result<ObjectMetadataUpdateResult>;

    async fn delete_object(&self, id: ServerId) -> Result<ObjectDeleteResult>;

    async fn move_object(
        &self,
        id: ServerId,
        folder_id: Option<FolderId>,
        owner: Owner,
        object_type: ObjectType,
    ) -> Result<bool>;

    async fn leave_object(&self, id: ServerId) -> Result<ObjectDeleteResult>;

    /// Fetches the last-used timestamps for all cloud environments.
    ///
    /// This is derived from `CloudEnvironment.lastTaskCreated.createdAt` (not `lastTaskRunTimestamp`)
    /// so that "Last used" reflects the most recently created task.
    ///
    /// Returns a map from environment UID to timestamp.
    async fn fetch_environment_last_task_run_timestamps(
        &self,
    ) -> Result<HashMap<String, DateTime<Utc>>>;
}

// ============================================================================
// OpenWarp 本地化 helpers
// ============================================================================

/// 合成 `RevisionAndLastEditor`,用当前时间作为 revision。OpenWarp 单机场景下
/// 服务端 revision 协调被砍,本地写入即最终态。`last_editor_uid = None` 避免
/// 让上层 `set_latest_revision_and_editor` 错误地把"当前编辑者"覆写成假数据。
fn synth_revision_and_editor() -> RevisionAndLastEditor {
    // `Revision::from(DateTime<Utc>)` 是 test-util feature gated;生产构建走
    // `From<ServerTimestamp>` 路径。
    RevisionAndLastEditor {
        revision: Revision::from(ServerTimestamp::new(Utc::now())),
        last_editor_uid: None,
    }
}

/// 合成 `ServerPermissions` —— 一个"个人空间,无共享,无 guest"的 default 状态。
/// OpenWarp 默认所有对象属于本地用户,不存在云端 sharing。
fn synth_personal_permissions() -> ServerPermissions {
    ServerPermissions {
        space: Owner::User {
            user_uid: crate::auth::UserUid::new(""),
        },
        guests: Vec::new(),
        anyone_link_sharing: None,
        permissions_last_updated_ts: ServerTimestamp::new(Utc::now()),
    }
}

/// 合成 `CreatedCloudObject`,用于 A 类 `create_*` 方法的成功路径。
/// 把 client_id 的 hash 提升为合成 server_id —— 但**这条 RPC 路径在 OpenWarp 不应
/// 真正被走到**:UpdateManager 已短路到 `create_object`(纯本地)。残留的调用方只有
/// `SyncQueue::create_object`(只在 `start_dequeueing` 触发时执行,本地化下队列基本
/// 为空)。合成响应只是为了让残留路径不死锁,不应有逻辑副作用。
fn synth_created_cloud_object(
    request: &CreateObjectRequest,
    id_type: ObjectIdType,
) -> CreatedCloudObject {
    // 从 client_id 派生 server_id:取 client_id 的 sqlite hash 字符串构造 ServerId。
    // 这与 UpdateManager 把"无 server_id 的本地对象"用 SyncId::ClientId 表示语义一致 ——
    // 这里强行升级到 server_id 仅是为了满足 trait 返回值需求,**不会**进入 CloudModel
    // 的 server_id 索引(因为 UpdateManager.create_object 早就用 ClientId 注册了对象)。
    let synthetic_id = ServerId::from_string_lossy(&request.client_id.to_hash());
    CreatedCloudObject {
        client_id: request.client_id,
        revision_and_editor: synth_revision_and_editor(),
        metadata_ts: ServerTimestamp::new(Utc::now()),
        server_id_and_type: ServerIdAndType {
            id: synthetic_id,
            id_type,
        },
        creator_uid: None,
        permissions: synth_personal_permissions(),
    }
}

// ============================================================================
// impl ObjectClient for ServerApi —— 全部本地化
// ============================================================================

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl ObjectClient for ServerApi {
    // ----- A 类:本地化合成成功响应 -----
    //
    // 调用图:
    //   `CloudWorkflowModel::send_create_request` → `object_client.create_workflow`
    //   仅来自 `UpdateManager::create_object_online`(OpenWarp 本地化后未调用)
    //   或 `SyncQueue::create_object`(本地化场景队列基本为空)。
    //   返回合成 Success 让任何残留路径走 success 分支。

    async fn create_workflow(
        &self,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult> {
        Ok(CreateCloudObjectResult::Success {
            created_cloud_object: synth_created_cloud_object(&request, ObjectIdType::Workflow),
        })
    }

    async fn update_workflow(
        &self,
        _workflow_id: WorkflowId,
        _data: SerializedModel,
        _revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<ServerWorkflow>> {
        // OpenWarp:仅来自 `update_object_online`(已短路)与 `SyncQueue::update_object`
        // (本地化下不入队 update)。返回 Success 是安全的"已成功保存到本地" 语义。
        Ok(UpdateCloudObjectResult::Success {
            revision_and_editor: synth_revision_and_editor(),
        })
    }

    async fn bulk_create_generic_string_objects(
        &self,
        _owner: Owner,
        objects: &[BulkCreateGenericStringObjectsRequest],
    ) -> Result<BulkCreateCloudObjectResult> {
        // OpenWarp:逐个合成 `CreatedCloudObject`。
        let created_cloud_objects = objects
            .iter()
            .map(|object| {
                let synthetic_id = ServerId::from_string_lossy(&object.id.to_hash());
                CreatedCloudObject {
                    client_id: object.id,
                    revision_and_editor: synth_revision_and_editor(),
                    metadata_ts: ServerTimestamp::new(Utc::now()),
                    server_id_and_type: ServerIdAndType {
                        id: synthetic_id,
                        id_type: ObjectIdType::GenericStringObject,
                    },
                    creator_uid: None,
                    permissions: synth_personal_permissions(),
                }
            })
            .collect();
        Ok(BulkCreateCloudObjectResult::Success {
            created_cloud_objects,
        })
    }

    async fn create_generic_string_object(
        &self,
        _format: GenericStringObjectFormat,
        _uniqueness_key: Option<GenericStringObjectUniqueKey>,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult> {
        Ok(CreateCloudObjectResult::Success {
            created_cloud_object: synth_created_cloud_object(
                &request,
                ObjectIdType::GenericStringObject,
            ),
        })
    }

    async fn create_notebook(
        &self,
        request: CreateObjectRequest,
    ) -> Result<CreateCloudObjectResult> {
        Ok(CreateCloudObjectResult::Success {
            created_cloud_object: synth_created_cloud_object(&request, ObjectIdType::Notebook),
        })
    }

    async fn update_notebook(
        &self,
        _notebook_id: NotebookId,
        _title: Option<String>,
        _data: Option<SerializedModel>,
        _revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<ServerNotebook>> {
        Ok(UpdateCloudObjectResult::Success {
            revision_and_editor: synth_revision_and_editor(),
        })
    }

    async fn create_folder(&self, request: CreateObjectRequest) -> Result<CreateCloudObjectResult> {
        Ok(CreateCloudObjectResult::Success {
            created_cloud_object: synth_created_cloud_object(&request, ObjectIdType::Folder),
        })
    }

    async fn update_folder(
        &self,
        _folder_id: FolderId,
        _name: SerializedModel,
    ) -> Result<UpdateCloudObjectResult<ServerFolder>> {
        Ok(UpdateCloudObjectResult::Success {
            revision_and_editor: synth_revision_and_editor(),
        })
    }

    async fn update_generic_string_object(
        &self,
        _object_id: GenericStringObjectId,
        _model: SerializedModel,
        _revision: Option<Revision>,
    ) -> Result<UpdateCloudObjectResult<Box<dyn ServerObject>>> {
        Ok(UpdateCloudObjectResult::Success {
            revision_and_editor: synth_revision_and_editor(),
        })
    }

    // ----- C 类:notebook 编辑权 —— Wave1-2 已 stub -----

    async fn grab_notebook_edit_access(&self, _notebook_id: NotebookId) -> Result<ServerMetadata> {
        // OpenWarp Wave1-2:本地单机无多人编辑锁(notebook editing baton),返回 Err
        // 让 UpdateManager 走 RequestFailed 分支。对于 optimistically_grant_access=true 的
        // 常见调用路径,该分支只记 warn 日志,本地乐观更新会保留(用户仍可编辑)。
        // 不能用 Ok(ServerMetadata::default()):RequestSucceeded 分支会调
        // `store_metadata_update` 拿返回的 metadata 覆写本地上下文编辑者 / trashed_ts /
        // folder_id / creator_uid,反而破坏本地状态。
        Err(anyhow!(
            "Notebook multi-editor lock is disabled in OpenWarp"
        ))
    }

    async fn give_up_notebook_edit_access(
        &self,
        _notebook_id: NotebookId,
    ) -> Result<ServerMetadata> {
        // OpenWarp Wave1-2:同上。与 grab_notebook_edit_access 对称。
        Err(anyhow!(
            "Notebook multi-editor lock is disabled in OpenWarp"
        ))
    }

    // ----- C 类:fetch_changed_objects / fetch_single_cloud_object -----

    async fn fetch_changed_objects(
        &self,
        _objects_to_update: ObjectsToUpdate,
        _force_refresh: bool,
    ) -> Result<InitialLoadResponse> {
        // OpenWarp Wave 2-1:无云端拉取,返回空响应。
        // 上层 `on_changed_objects_fetched` 会用空数据 mark `has_initial_load`,
        // 然后启动 SyncQueue dequeueing(队列本地化下为空 = no-op),完整链路平稳。
        Ok(InitialLoadResponse {
            updated_notebooks: Vec::new(),
            deleted_notebooks: Vec::new(),
            updated_workflows: Vec::new(),
            deleted_workflows: Vec::new(),
            updated_folders: Vec::new(),
            deleted_folders: Vec::new(),
            updated_generic_string_objects: HashMap::new(),
            deleted_generic_string_objects: Vec::new(),
            user_profiles: Vec::new(),
            action_histories: Vec::new(),
            mcp_gallery: Vec::new(),
        })
    }

    async fn fetch_single_cloud_object(&self, _id: ServerId) -> Result<GetCloudObjectResponse> {
        // OpenWarp Wave 2-1:无云端单对象拉取。调用方 (search / SharedWithMe UI) 在
        // 本地化下已无入口,Err 让残留调用直接 surface 错误而不是返回假数据。
        Err(anyhow!("Single cloud object fetch is disabled in OpenWarp"))
    }

    // ----- C 类:owner transfer —— no-op 成功 -----

    async fn transfer_notebook_owner(
        &self,
        _notebook_id: NotebookId,
        _owner: Owner,
    ) -> Result<bool> {
        // OpenWarp Wave 2-1:单机无 owner 概念,no-op Ok 让 UpdateManager 完成本地状态机。
        Ok(true)
    }

    async fn transfer_workflow_owner(
        &self,
        _workflow_id: WorkflowId,
        _owner: Owner,
    ) -> Result<bool> {
        Ok(true)
    }

    async fn transfer_generic_string_object_owner(
        &self,
        _gso_id: GenericStringObjectId,
        _owner: Owner,
    ) -> Result<bool> {
        Ok(true)
    }

    // ----- B 类:trash / untrash / delete / empty_trash / move -----
    //
    // UpdateManager 中有 server_id 的对象路径会走这条 RPC(本地化场景下偶发:
    // 旧云端同步过来的对象的 trash 操作)。返回 `Ok(成功)` 让上层 RequestSucceeded
    // 分支清 `has_pending_metadata_change`、写 sqlite,与"无 server_id"短路腿对齐。

    async fn trash_object(&self, _id: ServerId) -> Result<bool> {
        // OpenWarp:本地 trash 已由 UpdateManager 短路腿完成内存 + sqlite 写入。
        // 这里仅是带 server_id 对象走 RPC 时的"假成功",上层会清 pending 状态。
        Ok(true)
    }

    async fn untrash_object(&self, id: ServerId) -> Result<ObjectMetadataUpdateResult> {
        // OpenWarp:返回 Success + 合成 metadata。上层 RequestSucceeded 分支
        // 会用返回值更新本地内存。我们合成一个"清空 trashed_ts"的 metadata —
        // 这就是 untrash 的预期效果。
        Ok(ObjectMetadataUpdateResult::Success {
            metadata: Box::new(ServerMetadata {
                uid: id,
                revision: Revision::from(ServerTimestamp::new(Utc::now())),
                metadata_last_updated_ts: ServerTimestamp::new(Utc::now()),
                trashed_ts: None,
                folder_id: None,
                is_welcome_object: false,
                creator_uid: None,
                last_editor_uid: None,
                current_editor_uid: None,
            }),
        })
    }

    async fn delete_object(&self, id: ServerId) -> Result<ObjectDeleteResult> {
        // OpenWarp:仅返回单个 server_id 已删除。UpdateManager 的本地腿已经做了
        // 内存 + sqlite 删除,这里的 deleted_ids 用于上层 `on_object_delete_success`
        // 做 actions 清理与 ObjectOperationComplete 事件 emit。
        Ok(ObjectDeleteResult::Success {
            deleted_ids: vec![SyncId::ServerId(id)],
        })
    }

    async fn move_object(
        &self,
        _id: ServerId,
        _folder_id: Option<FolderId>,
        _owner: Owner,
        _object_type: ObjectType,
    ) -> Result<bool> {
        // OpenWarp:no-op 成功。上层 RequestSucceeded 分支清 pending 状态。
        Ok(true)
    }

    // ----- C 类:leave_object —— Wave1-2 已 stub -----

    async fn leave_object(&self, _id: ServerId) -> Result<ObjectDeleteResult> {
        // OpenWarp Wave1-2:云端 share 已下线,本地无法 leave 一个共享对象。
        // 返回 Err 让 UpdateManager 走 RequestFailed 分支,只弹失败 toast 不变本地 SQLite。
        // UI 入口 (DriveIndex::leave_object) 在本地机型下本不可达(无 share 对象)。
        Err(anyhow!("Leave shared object is disabled in OpenWarp"))
    }

    async fn fetch_environment_last_task_run_timestamps(
        &self,
    ) -> Result<HashMap<String, DateTime<Utc>>> {
        // OpenWarp Wave1-2:ambient agent 云端环境下线,本地不存在 cloud environment
        // "最后使用"时间戳。返回空表,UpdateManager::fetch_and_merge_environment_timestamps
        // 会以空 HashMap 走 update_environment_last_task_run_timestamps,无副作用。
        Ok(HashMap::new())
    }
}
