# lyrune-api

Lyrune 内部使用的 QQ 音乐协议库，不独立发布。

- `integration.rs`：应用使用的数据、凭据会话、扫码事件及刷新编排。
- `integration/protocol.rs`：QQ 曲库、搜索、歌单、歌词、音源与 CDN 请求。
- `login.rs` / `login/`：登录请求、响应校验和 MQTT 扫码状态机。
- `request.rs`：请求签名、哈希和协议标识算法。

应用通过 `integration` 调用具体能力。登录使用进程级 GUID，资源请求使用各自的凭据或 CDN 标识；这些标识和 Cookie 规则不能因代码相似而合并。

本库由 [AstronW/netease-qq-music-api](https://github.com/AstronW/netease-qq-music-api) 修改而来，已移除网易云及跨平台接口。`UPSTREAM_COMMIT`、`UPSTREAM_README.md` 和 `LICENSE` 保留上游来源与许可信息；上游 README 是历史资料，不代表当前接口。其他来源见根目录 `THIRD_PARTY_NOTICES.md`。
