# 代码结构与维护约束

重构采用 `1320e5d`（v1.5.0）的行为基线，没有引入 PR #35 的功能。模块拆分后继续做局部写法和重复逻辑整理，保持设置格式、缓存策略、任务生命周期及用户可见行为。

## 从哪里开始读

程序入口是 [main.rs](../crates/lyrune-app/src/main.rs)：创建 GPUI 应用和窗口，连接托盘、单实例和 Linux 媒体控制。[app.rs](../crates/lyrune-app/src/app.rs) 定义 `LyruneView`，创建控件和订阅，组合主界面。

`app/` 中的 `impl LyruneView` 都作用于同一个 GPUI 实体。字段集中在入口，方法按职责放置；关闭窗口后，主程序仍可持有这个实体，继续播放。拆分状态所有者之前，需要同时考虑窗口重开、订阅和后台任务的寿命。

当前阶段已确认保留文件布局和单个 `LyruneView`，先完善现有资源类型的职责。引入播放状态对象仍是待单独讨论的后续设计，当前布局不构成对最终状态划分的决定。

| 模块 | 负责什么 |
| --- | --- |
| [account](../crates/lyrune-app/src/app/account.rs) | 扫码登录、凭据恢复和刷新、退出及会话失效 |
| [navigation](../crates/lyrune-app/src/app/navigation.rs) | 页面快照、前进后退、资源共享、结果应用与淘汰 |
| [home](../crates/lyrune-app/src/app/home.rs)、[search](../crates/lyrune-app/src/app/search.rs)、[artist](../crates/lyrune-app/src/app/artist.rs) | 各页面的加载、分页和显示 |
| [playlists](../crates/lyrune-app/src/app/playlists.rs) | 用户歌单、分页浏览、喜欢状态及页面同步 |
| [queue](../crates/lyrune-app/src/app/queue.rs) | 播放队列的建立、插入、补全、恢复和推荐续播 |
| [playback](../crates/lyrune-app/src/app/playback.rs) | 音源选择、音质回退、解码准备、播放/暂停/跳歌/seek |
| [playback_sync](../crates/lyrune-app/src/app/playback_sync.rs) | 窗口进度、曲终处理、定期保存、MPRIS 和休眠抑制 |
| [player_view](../crates/lyrune-app/src/app/player_view.rs) | 播放栏、进度预览和音质菜单 |
| [lyrics_loading](../crates/lyrune-app/src/app/lyrics_loading.rs) | 歌词缓存读取、网络刷新与延迟采用新内容 |
| [lyrics](../crates/lyrune-app/src/app/lyrics.rs) | LRC/QRC 解析、翻译和注音对齐 |
| [lyrics_view](../crates/lyrune-app/src/app/lyrics_view.rs) | 歌词排版、逐字高亮、滚动、封面背景和动画帧调度 |
| [app/settings](../crates/lyrune-app/src/app/settings.rs) | 设置页面及偏好的即时生效 |

已有底层边界继续保留：`player.rs` 管音频设备与解码，`cache.rs` 管音频缓存与流，`http.rs` 管图片缓存，`settings.rs` 管设置和持久化数据格式，`credentials.rs` 管系统钥匙串，`library.rs` 管列表/表格 Delegate。QQ 协议实现仍在独立的 `qqmusic-api` crate。

## 关键调用关系

1. `LyruneView::new` 创建控件、接入事件，然后恢复账号。账号可用后加载首页和音乐库；音乐库加载还会触发已保存队列的恢复。
2. 导航先建立目标页及其资源，再记录当前页、应用目标页。搜索、歌手页面的请求完成后，由 `SearchResource` / `ArtistResource` 应用结果并结束相应加载状态；回调在释放资源锁后，按资源身份决定是否更新当前控件。资源方法不操作 GPUI，也不判断当前页面。
3. 歌单页和歌手页选曲共用 `select_playlist_track`，统一判断复用队列、切换暂停或建立新队列；搜索结果仍按独立规则插入当前歌曲之后。随后调用 `start_playback`，Tokio 任务通过 `prepare_playback_stream` 复用或重新解析音源，再到阻塞线程准备解码器；GPUI 回调检查播放版本后才调用 `AudioPlayer::replace`。
4. 后台 tick 读取音频进度、判断曲终、保存播放状态并同步系统媒体控制。歌词显示另有按设置帧率运行的动画回调。

## 容易误改的约束

- **浏览状态与播放状态独立。** `current_track`、`loading_track` 是播放队列索引；页面选中行不能直接当作队列索引。插入搜索歌曲会标记 `queue.modified`，此后不能仅凭原歌单 ID 复用队列。
- **队列追加保留已有歌曲。** `PlaybackQueue::append_tracks` 按 `mid` 去重，保留已有歌曲的位置和元数据；只返回第一首新增歌曲的索引，重复批次返回 `None`。页面播放标记通过 `playback_indices_in` 按歌曲身份映射，页面重新排序或缺少歌曲不会误用队列索引。
- **播放字段描述不同阶段。** `loading_autoplay` 在加载期间保存播放意图，连续切换音质时不能从已停止的设备推断。`resolving_qualities` 只表示音质列表解析阶段，结束后仍可能准备音频流或解码。`playback_started` 在暂停时保持为真；`active_quality` 是实际音质，可能低于持久化的用户偏好。
- **推荐预取与等待续播独立。** `queue_recommendation_loading` 可以在当前歌曲仍播放时为真；只有 `queue_waiting_for_recommendation` 才要求结果到达后从新增歌曲开始播放。
- **历史记录拥有页面资源。** 当前页、历史记录和进行中的请求持有 `Arc`，`PageResourceCache` 只保存 `Weak`。清空前进历史之前先取得目标资源，否则可能失去可复用的最后一个强引用。缓存键包含账号，结果是否属于当前页用 `Arc::ptr_eq` 判断。
- **补页失败保留已有结果。** 搜索的各类别可以同时补页，某类别成功不能清除其他类别的错误；歌手的歌曲、专辑各自结束加载并保留各自的错误。首次搜索成功会清除原搜索错误，歌手请求则在启动前清除对应错误，这些时机不同。请求前的检查还涉及历史页面的可见数量和通知，继续留在页面方法内。
- **分页可被两个用途共享。** 页面补页和播放队列补全通过 `SingleFlight` 共享同一账号、歌单、offset 的请求。资源只追加期望的下一页，避免重复回填。队列切换后，旧任务仍可能补全自己持有的页面资源，但不能覆盖新队列。
- **失效与取消是两件事。** 登录、音乐库、首页、队列和音源加载分别使用版本计数器。它们拒绝晚到的结果，并不会自动终止网络请求。凭据刷新还比较会话身份，防止同一账号的旧会话影响新会话。
- **两套 tick 的寿命不同。** `spawn_in` 的窗口任务更新进度控件；`spawn` 的应用任务在窗口关闭后仍处理曲终与保存。暂停时等待通道唤醒。窗口重开通过 `attach_window` 重建窗口相关同步。
- **歌词有多层缓存。** 原始磁盘缓存、已解析内容、排版结果和当前帧面板各自解决不同问题。已有歌词正在播放时，网络刷新得到的不同内容先进入 `pending_lyrics_cache`，以后选中该曲时采用，避免本次播放中时间轴突然变化。
- **歌词时间和排版不能按字符数简化。** 字范围是 UTF-8 字节下标；没有字级计时的标点和尾部文字借用相邻时间段。QRC 的严格与宽松解析都覆盖真实格式差异，现有测试包含未转义属性和截断输入。
- **自绘和帧率缓存有用途。** 歌词缩放复用已排版字形；进度条用路径避免设备像素取整造成跳动。面板缓存让其他控件重绘时仍遵守歌词帧率限制。`expanded`、`visible`、`fully_expanded` 分别表示用户目标、过渡可见性和跳过底层页面渲染的时机。

网络与阻塞准备任务使用 [runtime.rs](../crates/lyrune-app/src/runtime.rs) 的共享 Tokio runtime；GPUI 实体通过 `cx.spawn` / `this.update` 接收结果。`drop(RUNTIME.spawn(...))` 让 Tokio 任务继续运行，不能理解为取消。长期维护任务保存 `JoinHandle`，重启维护或实体销毁时显式 `abort`。

## 保留、以后再判断的代码

| 位置 | 当前证据与下一步需要的证据 |
| --- | --- |
| `ProtocolClient::playback_url` | 应用路径使用 `playback_options`；公共 API 不能仅凭当前应用无调用就删除，还需确认 crate 的外部使用和兼容要求。 |
| `SearchCardGridDelegate::set_cards` 的参数列表 | Clippy 报告参数过多。参数分别描述数据、布局和分页状态，目前只剩一个调用点；暂未为消除提示引入参数对象，也未关闭提示。后续若布局需要独立复用，再评估其边界。 |
| `LyruneView::new` 和设置页的长函数 | 初始化仍集中展示控件、订阅、默认值和启动顺序。三处相同的补页监听已合并；剩余长度本身不足以证明需要新的对象或更多单次调用的函数。 |
| `AudioCache::prepare_for_seek` | 仅在测试中使用，覆盖跳转 Range 请求且不发送 HEAD 的路径；删除前需要替代这项覆盖，并重新判断 `validate_remote` 的用途。 |
| `LyruneView::drop` 的两次保存调用 | `persist_current_playback` 可能提前返回，且仅在播放快照变化时写设置；后面的 `persist_settings` 仍会保存其他偏好，不能直接作为重复调用删除。 |

上述代码仍保留。歌手页原有的多类别卡片函数已确认只有专辑调用，现收窄为 `render_artist_albums`；专辑分支的布局与交互保持原样。搜索页三类卡片共用列表更新流程，各类别仍保留自己的数据、分页状态和卡片高度。歌单和搜索表格使用同一个构造入口，表头高度与内边距保持原值。音质回退、过期结果处理、失败跳歌和持久化时机保持不变。

另有一项需要单独复现的行为风险：`queue::start_home_recommendation` 回填时只比较推荐类别，未比较账号或请求版本。若旧账号请求跨越退出、重新登录和相同类别的新请求，类别比较可能无法区分结果。需要先用受控的返回顺序确认，再作为行为修复处理，本轮保留基线逻辑。

## 验证

```sh
cargo check --locked -p lyrune --all-targets
cargo test --locked -p lyrune
cargo test --locked -p netease-qq-music-api quality_fallback
cargo clippy --locked -p lyrune --all-targets
cargo fmt --all -- --check
```

应用回归测试在 [app/tests.rs](../crates/lyrune-app/src/app/tests.rs)，纯解析测试在 `app/lyrics.rs`。现有测试覆盖页面资源寿命、历史滚动位置、队列插入、歌词计时和缓存等关键约束。部分缓存测试启动本机 HTTP 服务，需要允许绑定回环端口；两个真实账号测试默认忽略，需要系统钥匙串中的凭据和 QQ 音乐网络访问。

局部整理复用已有测试，没有为模式匹配、构造入口或信号转发增加测试。队列规则整理新增两项行为测试，验证追加顺序、重复歌曲保留与续播起点，以及页面重新排序或缺少歌曲时的播放标记。音源准备与回调整理保留回退分支、事件发送顺序和播放版本过滤；音源获取与设备播放失败共用提示和跳歌处理，各自的错误文案不变。现有音质和缓存测试覆盖各自策略，不能单独证明完整的播放交互链路。

页面资源整理新增两项行为测试，覆盖首次搜索失败后的结果应用、不同搜索类别的补页隔离，以及歌手分页失败后的数据保留、返回偏移、总数更新和首批替换。四个结果处理块先原样移入现有资源类型，展开方法后与阶段起点 `e06ac06` 的函数体一致（忽略注释和格式）；随后将锁变量和显式 `drop` 简化为单条方法调用，临时锁守卫在语句结束时释放，仍先于页面身份检查和通知。测试只验证资源状态规则，未替代真实页面切换和后台回填的桌面验收。

以下行为还需要真实桌面环境检查，自动测试通过不能替代它们：

- 登录、退出、重启恢复凭据，加载期间切换账号或页面。
- 搜索和歌单前进后退、恢复滚动位置，同时保持原队列播放。
- 快速切歌、切音质、拖动进度、暂停续播和推荐队尾续播。
- 歌词展开/收起、字体与帧率切换、切歌时封面和歌词过渡。
- 有托盘时关闭并重开窗口，媒体键/MPRIS 控制和退出后的队列恢复。
