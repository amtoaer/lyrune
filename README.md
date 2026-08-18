<p align="center">
  <img src="crates/lyrune-app/assets/lyrune.svg" width="96" alt="Lyrune icon">
</p>

<h1 align="center">Lyrune</h1>

<p align="center">
  <strong>使用 Rust 和 GPUI / GPUI Component 构建的 Linux QQ 音乐桌面客户端</strong>
</p>

![Lyrune](docs/screenshots/lyrune.webp)

## 背景

> [!WARNING]
> 虽不排除我后续会抽空 review，但截至撰写此 README，该项目仍然是一个完全 vibe 的作品。
>
> 我不会对自己未实际检查过的代码的质量和功能做任何担保——**Use it at your own risk**.

最近从 Spotify 切到了 QQ 音乐，发现 QQ 音乐 For Linux 相当难用：

1. 基于 Electron，占用偏高；
2. 不显示也不支持切换音质，难以使用；
3. Mpris 功能极度残缺，上报数据不完整，且暂停、播放等媒体控制不可用。

正好手头有 Codex，心血来潮试了试完全的 Vibe，体验比预期好很多：

+ UI 设计部分大量参考了 Spotify，与 Open Design 勾兑后由其转换为具体 Prompt 交由 Codex 执行；

+ 代码全部由 GPT 5.6 Sol Extra High 编写，截至 1.0.0 发布总耗时三四天，消耗 ChatGPT Pro 20x 七日限额的 50%左右。

## 功能

包含了我对音乐播放器的全部需求。

1. 扫码登录，加载“我喜欢”和用户歌单，双击播放；
2. 自由选择音质，缓存播过的曲目防止多次请求；
3. 支持某种程度的智能歌单（QQ 音乐的“专属雷达”、“猜你喜欢”）；
4. 支持搜索，查看单曲、歌单、专辑、歌手；
5. 支持较为美观的歌词展示，支持修改播放器主题、字体；
6. 支持托盘，关闭程序后仍在后台播放；
7. 完全不依赖 Webview，运行流畅、占用较低。


## 外观

### 主页

![主页](docs/screenshots/home.webp)

### 歌单

![歌单](docs/screenshots/playlist.webp)

### 搜索

![搜索](docs/screenshots/search.webp)

### 歌词

![歌词](docs/screenshots/lyrics.webp)

### 设置

![设置](docs/screenshots/settings.webp)

## 平台支持

该项目的唯一目的是提高 Linux 用户的 QQ 音乐使用体验，因此仅会在 Release 中包含 Linux 可执行程序，无意发布 Windows / Mac 产物。

此外虽然程序使用跨平台的语言和 UI 框架，Windows / Mac 可以无障碍自行编译，但个人仍然不推荐。

这是因为该项目使用的 GPUI 框架缺失 Damage Tracking，在高强度重绘的歌词页占用不甚理想。我目前通过自行 fork GPUI 合并上游 [针对 Linux 的 Damage Tracking](https://github.com/zed-industries/zed/pull/62455) 与 [Wayland 渲染改进](https://github.com/zed-industries/zed/pull/60690) 在 Linux 上基本解决了该性能问题，但由于Windows / Mac 没有对应的 Damage Tracking 实现，预期性能较 Linux 差不少。

当然如果非常想用，设置里也提供了歌词页的动画帧率限制，可以以牺牲流畅度为代价大幅改善体验。

## 参考与致谢

该项目实现过程中参考了如下项目的逻辑：

+ [AstronW/netease-qq-music-api](https://github.com/AstronW/netease-qq-music-api)
+ [L-1124/QQMusicApi](https://github.com/L-1124/QQMusicApi)
+ [yakult-green-tea/qq-music-api](https://github.com/yakult-green-tea/qq-music-api)
+ [CharlesPikachu/musicdl](https://github.com/CharlesPikachu/musicdl)
+ [Yyyangshenghao/simple-music](https://github.com/Yyyangshenghao/simple-music)
+ [jixunmoe-go/qrc](https://github.com/jixunmoe-go/qrc)
+ [WXRIW/Lyricify-Lyrics-Helper](https://github.com/WXRIW/Lyricify-Lyrics-Helper)
+ [christosk92/WaveeMusic](https://github.com/christosk92/WaveeMusic)
+ ...

感谢他们的贡献。

## 免责声明

Lyrune 是非官方客户端，与腾讯及 QQ 音乐无隶属或认可关系。请遵守当地法律、QQ 音乐服务条款及内容授权要求。
