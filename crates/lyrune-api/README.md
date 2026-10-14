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
- `signing.rs`：登录与资源请求共用的签名、哈希；登录 GUID 和搜索 ID 由各自模块生成。

应用从 crate 根入口调用具体能力。登录使用进程级 GUID，资源请求使用各自的凭据或 CDN 标识；这些标识和 Cookie 规则不能因代码相似而合并。

响应处理先检查 HTTP、网关和业务错误，再取出成功数据。歌词与播放条目使用私有 Serde 响应结构体，保持数字/字符串和非法字段兼容；它们不替换 `Track`、`QqCredential`、`LyricResult` 等本地序列化模型。

曲库与搜索保留动态解析：同一数据存在不同包装、多别名优先级和不同的坏条目策略。用户资料及歌单详情还依赖递归查找；CDN 合并 `sip` / `sipinfo` 并保留现有顺序和策略字段优先级。没有充分样本证明这些规则可以收窄。`response.rs` 记录共用规则，具体过滤、分页和回退仍由各领域负责。

本库由 [AstronW/netease-qq-music-api](https://github.com/AstronW/netease-qq-music-api) 修改而来，已移除网易云及跨平台接口。`UPSTREAM_COMMIT`、`UPSTREAM_README.md` 和 `LICENSE` 保留上游来源与许可信息；上游 README 是历史资料，不代表当前接口。其他来源见根目录 `THIRD_PARTY_NOTICES.md`。
