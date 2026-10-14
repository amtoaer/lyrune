# lyrune-api

Lyrune 内部使用的 QQ 音乐协议库，不独立发布。

- `lib.rs`：应用入口及选择性导出。
- `account.rs`：凭据会话、扫码事件及刷新编排。
- `models.rs`：应用使用的曲库、搜索、播放和持久化数据类型。
- `client.rs`：HTTP 客户端、签名网关请求及错误检查。
- `client/library.rs` / `search.rs`：曲库、推荐、资料、搜索与分页。
- `client/playback.rs` / `cdn.rs`：音源解析、CDN 调度与候选地址。
- `client/lyrics.rs` / `lyrics/qrc_des.rs`：歌词请求、解码和 QRC 算法。
- `client/response.rs`：现有字段兼容及递归查找规则。
- `login.rs` / `login/`：登录请求、响应校验和 MQTT 扫码状态机。
- `request.rs`：请求签名、哈希和协议标识算法。

应用从 crate 根入口调用具体能力。登录使用进程级 GUID，资源请求使用各自的凭据或 CDN 标识；这些标识和 Cookie 规则不能因代码相似而合并。

本库由 [AstronW/netease-qq-music-api](https://github.com/AstronW/netease-qq-music-api) 修改而来，已移除网易云及跨平台接口。`UPSTREAM_COMMIT`、`UPSTREAM_README.md` 和 `LICENSE` 保留上游来源与许可信息；上游 README 是历史资料，不代表当前接口。其他来源见根目录 `THIRD_PARTY_NOTICES.md`。
