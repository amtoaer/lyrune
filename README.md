# Lyrune

Lyrune 是一个使用官方 [GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui) 与
[gpui-component](https://github.com/longbridge/gpui-component) 构建的桌面音乐播放器原型。

当前原型只验证 QQ 音乐链路：

- QQ 音乐 App 扫码登录
- 使用系统钥匙串保存登录凭据
- 加载“我喜欢”歌曲
- 边下载、边缓存、边播放
- 支持部分缓存的 HTTP Range 断点续传
- 128 kbps、320 kbps、FLAC 三档音质切换

## 运行

```bash
cargo run --release
```

Linux 需要可用的 Secret Service（例如 GNOME Keyring 或 KWallet）以及系统音频输出。
如果系统钥匙串不可用，应用会保留本次登录供当前进程使用，但不会把凭据降级保存为明文。

## 原型边界

- 音频以 provider、歌曲 `mid`、媒体 `media_mid` 和音质生成的哈希作为内部缓存键，
  不保存临时播放 URL，也不会写入可直接导出的音乐标签。
- 播放会预缓冲 1 MiB；切歌时保留已下载前缀，下次播放通过 ETag、Last-Modified、
  内容长度和前缀探测校验后使用 HTTP Range 续传。
- 当前尚未实现缓存容量上限和 LRU 清理。
- QQ 音乐接口是未公开协议，可能随上游调整而变化。
- 部分音质需要对应的会员权益；没有权限时应用会显示上游未返回可用地址。
- 当前只实现 QQ 音乐 App 扫码，不包含微信扫码。

## Workspace

- `crates/lyrune-app`：GPUI 桌面应用与播放逻辑。
- `crates/qqmusic-api`：内置的 QQ 音乐协议实现；基于
  [`netease-qq-music-api`](https://github.com/AstronW/netease-qq-music-api)
  修改，不需要额外启动 Node.js 服务。
- 第三方依赖的版本和 feature 统一声明在根目录 `Cargo.toml`，workspace
  成员通过 `workspace = true` 引用。
- GPUI 使用 Zed revision `24e25552b1259d56a6fdd7956a419ed9e8a1a25e`，
  gpui-component 使用 revision `56f3d903eaef3b2504cf31518ed1ad69d80166ff`。
  前者为保持与 gpui-component 内部依赖的 Cargo source 一致，由 `Cargo.lock`
  精确锁定；不要在更新依赖时无意升级该 Git source。

第三方代码与协议实现的来源说明见 [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md)。
