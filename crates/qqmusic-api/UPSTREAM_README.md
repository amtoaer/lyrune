# netease-qq-music-api

统一的异步音乐 API 封装库，提供网易云音乐与 QQ 音乐的一致化调用接口。

- 统一入口：`MusicClient`
- 统一模型：`models`
- 统一错误：`MusicClientError`
- 调用风格：typed builder（链式构建 + 编译期类型态约束）

---

> [!IMPORTANT]
> 本库仅提供音乐平台 API 调用封装，不提供、存储或分发任何音乐内容，也不授予任何平台内容、账号、数据或版权的使用权。
> 请遵守当地法律法规及平台服务条款，尊重版权并支持正版。
> 使用者应自行确保其使用方式符合适用法律法规、平台服务条款和权利人授权。
> 请勿将本库用于绕过付费/会员限制、批量下载、未授权传播或再分发音乐内容。
> 因使用或无法使用本项目产生的风险与责任，由使用者自行承担。

---

## 适用场景

- 同时接入网易云与 QQ 音乐，但不想维护两套 API 适配层
- 需要统一的搜索、详情、播放、发现、歌单与登录流程
- 希望以强类型和一致错误语义组织上层业务代码

## 安装

```toml
[dependencies]
netease-qq-music-api = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "time"] }
```

## 快速开始

```rust
use netease_qq_music_api::MusicClient;
use netease_qq_music_api::models::Platform;

#[tokio::main]
async fn main() -> Result<(), netease_qq_music_api::MusicClientError> {
    let client = MusicClient::new();

    let result = client
        .search()
        .song()
        .keyword("江南")
        .platform(Platform::Netease)
        .limit(10)
        .send()
        .await?;

    println!("songs: {}", result.songs.len());
    Ok(())
}
```

## 核心规则

- 默认平台是 `Platform::Netease`，可通过 `.platform(Platform::Tencent)` 切换
- 所有请求最终都通过 `.send().await` 发起
- 支持鉴权的请求可通过 `.login(&token)` 注入登录态
- `detail` / `playback` 的 `id(...)` 支持 `impl MusicIdInput`（可传 `u64`、`String`、`&str`）
- `detail.playlist()`、`detail.toplist()`、`playlist.detail()` 在发送前都会校验 `id` 必须是纯数字字符串

## API 速查

| 域         | 子能力                                                                   | 必填参数      | 可选参数                               | 返回类型                                                                                      |
| ---------- | ------------------------------------------------------------------------ | ------------- | -------------------------------------- | --------------------------------------------------------------------------------------------- |
| `search`   | `song` / `artist` / `album` / `playlist`                                 | `keyword`     | `offset`, `limit`, `platform`, `login` | `SearchSongResult` / `SearchArtistResult` / `SearchAlbumResult` / `SearchPlaylistResult`      |
| `detail`   | `song`                                                                   | `id` 或 `ids` | `platform`, `login`                    | `SongsDetailResult`                                                                           |
| `detail`   | `artist`                                                                 | `id`          | `offset`, `limit`, `platform`, `login` | `ArtistDetailResult`                                                                          |
| `detail`   | `album` / `playlist` \| `toplist`                                        | `id`          | `platform`, `login`                    | `AlbumDetailResult` / `PlaylistDetailResult`                                                  |
| `playback` | `lyric`                                                                  | `id`          | `platform`, `login`                    | `LyricResult`                                                                                 |
| `playback` | `url`                                                                    | `id`          | `level`, `platform`, `login`           | `UrlResult`                                                                                   |
| `discover` | `suggests`                                                               | `keyword`     | `platform`, `login`                    | `SearchSuggestResult`                                                                         |
| `discover` | `hotkey` / `recommend_playlist` / `toplist_list` / `playlist_categories` | 无            | `platform`, `login`                    | `HotkeyResult` / `RecommendPlaylistResult` / `ToplistListResult` / `PlaylistCategoriesResult` |
| `discover` | `playlist_list`                                                          | `category`    | `offset`, `limit`, `platform`, `login` | `PlaylistListResult`                                                                          |
| `playlist` | `detail`                                                                 | `id`          | `platform`, `login`                    | `PlaylistDetailResult`                                                                        |
| `playlist` | `categories`                                                             | 无            | `platform`, `login`                    | `PlaylistCategoriesResult`                                                                    |
| `playlist` | `list`                                                                   | `category`    | `offset`, `limit`, `platform`, `login` | `PlaylistListResult`                                                                          |
| `login`    | `session`                                                                | 无            | `platform`                             | `LoginSession`                                                                                |
| `login`    | `refresh`                                                                | `token`       | `platform`                             | `LoginToken`                                                                                  |

## 使用示例

### 搜索

```rust
use netease_qq_music_api::MusicClient;
use netease_qq_music_api::models::Platform;

async fn demo(client: &MusicClient) -> Result<(), netease_qq_music_api::MusicClientError> {
    let songs = client
        .search()
        .song()
        .keyword("林俊杰")
        .platform(Platform::Tencent)
        .limit(20)
        .offset(0)
        .send()
        .await?;

    println!("first song id: {}", songs.songs[0].id);
    Ok(())
}
```

### 详情与播放

```rust
use netease_qq_music_api::MusicClient;
use netease_qq_music_api::models::{Platform, SongQuality};

async fn demo(client: &MusicClient) -> Result<(), netease_qq_music_api::MusicClientError> {
    let artist = client
        .detail()
        .artist()
        .id("3684")
        .platform(Platform::Netease)
        .limit(10)
        .send()
        .await?;

    let song = client
        .detail()
        .song()
        .id(108914u64)
        .platform(Platform::Netease)
        .send()
        .await?;

    let url = client
        .playback()
        .url()
        .id(song.songs[0].id.as_str())
        .level(SongQuality::Lossless)
        .send()
        .await?;

    println!("artist: {}, play url: {}", artist.name, url.url);
    Ok(())
}
```

### 发现与歌单

```rust
use netease_qq_music_api::MusicClient;
use netease_qq_music_api::models::Platform;

async fn demo(client: &MusicClient) -> Result<(), netease_qq_music_api::MusicClientError> {
    let suggests = client
        .discover()
        .suggests()
        .keyword("周杰伦")
        .platform(Platform::Tencent)
        .send()
        .await?;

    let categories = client
        .playlist()
        .categories()
        .platform(Platform::Netease)
        .send()
        .await?;

    println!("suggest count: {}", suggests.suggests.len());
    println!("category groups: {}", categories.group.len());
    Ok(())
}
```

### 二维码登录与轮询状态

```rust
use netease_qq_music_api::MusicClient;
use netease_qq_music_api::models::{LoginStatus, Platform};

async fn login_demo(client: &MusicClient) -> Result<(), netease_qq_music_api::MusicClientError> {
    let session = client
        .login()
        .session()
        .platform(Platform::Tencent)
        .send()
        .await?;

    println!("qr data url: {}", session.qr_code());

    loop {
        match session.status().await? {
            LoginStatus::Success(_) => {
                println!("login success");
                break;
            }
            LoginStatus::QrCodeExpired => {
                println!("qr expired, recreate session");
                break;
            }
            LoginStatus::WaitingScan => println!("waiting scan"),
            LoginStatus::WaitingConfirm => println!("waiting confirm"),
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }

    Ok(())
}
```

### 刷新登录态

```rust
use netease_qq_music_api::MusicClient;
use netease_qq_music_api::models::{LoginToken, Platform, TencentLoginToken};

async fn refresh_demo(client: &MusicClient) -> Result<(), netease_qq_music_api::MusicClientError> {
    let token = TencentLoginToken::new(
        123456,
        "music_key",
        "refresh_token",
        "refresh_key",
        None,
        1,
    );

    let refreshed = client
        .login()
        .refresh()
        .platform(Platform::Tencent)
        .token(&token)
        .send()
        .await?;

    if let LoginToken::Tencent(new_token) = refreshed {
        println!("new expires_at: {:?}", new_token.expires_at());
    }

    Ok(())
}
```

## 错误处理建议

常见错误分组：

- 参数缺失：`MissingKeyword`、`MissingId`、`MissingCategory`、`MissingRefreshToken`
- 鉴权不匹配：`AuthTokenPlatformMismatch`
- 登录链路：`TencentLoginCanceled`、`TencentLoginFailed`、`TencentMqttLogin` 等
- 网络错误：`NetworkError`

示例：

```rust
use netease_qq_music_api::MusicClient;
use netease_qq_music_api::error::MusicClientError;

async fn error_demo(client: &MusicClient) {
    let result = client.search().song().send().await;
    match result {
        Ok(_) => {}
        Err(MusicClientError::MissingKeyword) => {
            eprintln!("please provide keyword");
        }
        Err(err) => {
            eprintln!("request failed: {err}");
        }
    }
}
```

## 文档与链接

- Crates.io: <https://crates.io/crates/netease-qq-music-api>
- Docs.rs: <https://docs.rs/netease-qq-music-api>
- Repository: <https://github.com/AstronW/netease-qq-music-api>

## 参考与致谢

本项目在接口行为理解与字段映射过程中参考了以下项目：

- [QQMusicApi](https://github.com/L-1124/QQMusicApi)
- [NeteaseCloudMusicApi](https://github.com/Binaryify/NeteaseCloudMusicApi)

## License

MIT
