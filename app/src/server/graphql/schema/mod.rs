pub mod util;

// OpenWarp Wave 2-1:原有两个 TryFrom impl
//   - `UpdateGenericStringObjectResult` → `UpdateCloudObjectResult<Box<dyn ServerObject>>`
//   - `ObjectUpdateSuccess` → `UpdateCloudObjectResult<ServerFolder>`
// 唯一消费方是 `ObjectClient::update_generic_string_object` / `update_folder` 的
// GraphQL 路径。本 Wave 把这两个方法本地化为合成 Success 后,impl 变 dead code,
// 一并物理删除以拿走对 `warp_graphql::mutations::update_generic_string_object`
// 与 `warp_graphql::object::ObjectUpdateSuccess` 的依赖,使 21 个 mutation 文件
// 可以物理移除。`util` 子模块的 GraphQL ↔ 本地类型转换仍被本地化路径以外的
// 残余消费方(actions 解析等)间接引用,保留待 Wave 3 一起清理。
