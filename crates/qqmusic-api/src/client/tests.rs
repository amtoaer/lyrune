use super::*;
use crate::error::MusicClientError;
use crate::models::{Platform, SongQuality};

mod section_validation {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn search_requires_keyword() {
        let client = MusicClient::new();
        let err = client.search().send().await.expect_err("missing keyword should fail");
        assert!(matches!(err, MusicClientError::MissingKeyword));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_rejects_empty_keyword() {
        let client = MusicClient::new();
        let err =
            client.search().keyword("   ").send().await.expect_err("empty keyword should fail");
        assert!(matches!(err, MusicClientError::MissingKeyword));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detail_requires_id() {
        let client = MusicClient::new();
        let err = client.detail().artist().send().await.expect_err("missing id should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detail_rejects_empty_id() {
        let client = MusicClient::new();
        let err =
            client.detail().artist().id("   ").send().await.expect_err("empty id should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detail_album_rejects_empty_id() {
        let client = MusicClient::new();
        let err = client.detail().album().id("").send().await.expect_err("empty id should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detail_toplist_requires_id() {
        let client = MusicClient::new();
        let err = client.detail().toplist().send().await.expect_err("missing id should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detail_playlist_rejects_non_digit_id() {
        let client = MusicClient::new();
        let err = client
            .detail()
            .playlist()
            .id("playlist-id")
            .send()
            .await
            .expect_err("non-digit id should fail");
        assert!(matches!(err, MusicClientError::InvalidIdFormat(value) if value == "playlist-id"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detail_toplist_rejects_non_digit_id() {
        let client = MusicClient::new();
        let err = client
            .detail()
            .toplist()
            .id("toplist-id")
            .send()
            .await
            .expect_err("non-digit id should fail");
        assert!(matches!(err, MusicClientError::InvalidIdFormat(value) if value == "toplist-id"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detail_song_requires_id() {
        let client = MusicClient::new();
        let err = client.detail().song().send().await.expect_err("missing id should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detail_song_rejects_empty_ids() {
        let client = MusicClient::new();
        let err = client
            .detail()
            .song()
            .ids(vec!["".to_string(), "   ".to_string()])
            .send()
            .await
            .expect_err("empty ids should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discover_playlist_list_requires_category() {
        let client = MusicClient::new();
        let err = client
            .discover()
            .playlist_list()
            .send()
            .await
            .expect_err("missing category should fail");
        assert!(matches!(err, MusicClientError::MissingCategory));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn discover_playlist_list_rejects_empty_category() {
        let client = MusicClient::new();
        let err = client
            .discover()
            .playlist_list()
            .category(" ")
            .send()
            .await
            .expect_err("empty category should fail");
        assert!(matches!(err, MusicClientError::MissingCategory));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn search_suggests_requires_keyword() {
        let client = MusicClient::new();
        let err =
            client.discover().suggests().send().await.expect_err("missing keyword should fail");
        assert!(matches!(err, MusicClientError::MissingKeyword));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auth_refresh_requires_token() {
        let client = MusicClient::new();
        let err =
            client.login().refresh().send().await.expect_err("missing refresh token should fail");
        assert!(matches!(err, MusicClientError::MissingRefreshToken));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playlist_detail_requires_id() {
        let client = MusicClient::new();
        let err = client.playlist().detail().send().await.expect_err("missing id should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playlist_detail_rejects_non_digit_id() {
        let client = MusicClient::new();
        let err = client
            .playlist()
            .detail()
            .id("abc")
            .send()
            .await
            .expect_err("non-digit id should fail");
        assert!(matches!(err, MusicClientError::InvalidIdFormat(value) if value == "abc"));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playlist_list_requires_category() {
        let client = MusicClient::new();
        let err = client.playlist().list().send().await.expect_err("missing category should fail");
        assert!(matches!(err, MusicClientError::MissingCategory));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playlist_list_rejects_empty_category() {
        let client = MusicClient::new();
        let err = client
            .playlist()
            .list()
            .category("")
            .send()
            .await
            .expect_err("empty category should fail");
        assert!(matches!(err, MusicClientError::MissingCategory));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playback_lyric_rejects_empty_id() {
        let client = MusicClient::new();
        let err = client.playback().lyric().id("").send().await.expect_err("empty id should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playback_url_rejects_empty_id() {
        let client = MusicClient::new();
        let err = client.playback().url().id(" ").send().await.expect_err("empty id should fail");
        assert!(matches!(err, MusicClientError::MissingId));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn playlist_list_rejects_invalid_tencent_category() {
        let client = MusicClient::new();
        let err = client
            .playlist()
            .list()
            .platform(Platform::Tencent)
            .category("not-a-number")
            .send()
            .await
            .expect_err("invalid category should fail");
        assert!(
            matches!(err, MusicClientError::InvalidCategoryId(category) if category == "not-a-number")
        );
    }
}

mod section_live_smoke {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_search_song_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .search()
            .keyword("江南")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("search should succeed");
        assert_eq!(response.songs[0].id, "108914");
        assert_eq!(response.songs[0].album.id, "10804");
        assert_eq!(response.songs[0].album.name, "第二天堂");
        assert_eq!(response.songs[0].artists[0].name, "林俊杰");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_search_song_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .search()
            .keyword("江南")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("search should succeed");
        assert_eq!(response.songs[0].id, "004TXEXY2G2c7C");
        assert_eq!(response.songs[0].album.id, "000y5gq7449K9I");
        assert_eq!(response.songs[0].album.name, "第二天堂");
        assert_eq!(response.songs[0].artists[0].name, "林俊杰");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_search_artist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .search()
            .artist()
            .keyword("林俊杰")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("search should succeed");
        assert_eq!(response.artists[0].id, "3684");
        assert_eq!(response.artists[0].name, "林俊杰");
        assert!(!response.artists[0].pic_url.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_search_artist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .search()
            .artist()
            .keyword("林俊杰")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("search should succeed");
        assert_eq!(response.artists[0].id, "001BLpXF2DyJe2");
        assert_eq!(response.artists[0].name, "林俊杰");
        assert!(!response.artists[0].pic_url.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_search_album_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .search()
            .album()
            .keyword("江南")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("search should succeed");
        assert_eq!(response.albums[0].id, "10804");
        assert_eq!(response.albums[0].name, "第二天堂");
        assert!(!response.albums[0].pic_url.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_search_album_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .search()
            .album()
            .keyword("江南")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("search should succeed");
        assert_eq!(response.albums[0].id, "000y5gq7449K9I");
        assert_eq!(response.albums[0].name, "第二天堂");
        assert!(!response.albums[0].pic_url.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_search_playlist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .search()
            .playlist()
            .keyword("江南")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("search should succeed");
        assert_eq!(response.playlists[0].id, "8814710412");
        assert_eq!(response.playlists[0].name, "烟雨江南 | 你一句春不晚 我到了真江南");
        assert!(!response.playlists[0].pic_url.is_empty());
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_search_playlist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .search()
            .playlist()
            .keyword("华语歌曲粤语版：除了“江南”还有这些歌")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("search should succeed");
        assert_eq!(response.playlists[0].id, "2665887142");
        assert_eq!(response.playlists[0].name, "华语歌曲粤语版：除了“江南”还有这些歌");
        assert!(response.playlists[0].pic_url.starts_with("http://qpic.y.qq.com/music_cover/"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_detail_songs_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .song()
            .id("1392942840")
            .ids(vec!["1859245776", "1862710424"])
            .platform(Platform::Netease)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.songs[0].id, "1392942840");
        assert_eq!(response.songs[0].album.id, "81878288");
        assert_eq!(response.songs[0].album.name, "烟雨行舟");
        assert_eq!(response.songs[0].artists[0].id, "28863695");
        assert_eq!(response.songs[1].id, "1859245776");
        assert_eq!(response.songs[1].album.id, "130016223");
        assert_eq!(response.songs[1].album.name, "STAY");
        assert_eq!(response.songs[1].artists[0].id, "32795025");
        assert_eq!(response.songs[2].id, "1862710424");
        assert_eq!(response.songs[2].album.id, "130631571");
        assert_eq!(response.songs[2].artists[0].id, "32795025");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_detail_songs_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .song()
            .id("001N8e5Q4Gjxda")
            .ids(vec!["004Gq0xE1YC8xp", "00264YJC1bmylC"])
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.songs[0].id, "001N8e5Q4Gjxda");
        assert_eq!(response.songs[0].album.id, "002g6zv02X7SNi");
        assert_eq!(response.songs[0].album.name, "JJ陆");
        assert_eq!(response.songs[0].artists[0].id, "001BLpXF2DyJe2");
        assert_eq!(response.songs[1].id, "004Gq0xE1YC8xp");
        assert_eq!(response.songs[1].album.id, "0035f8nw11cjkf");
        assert_eq!(response.songs[1].album.name, "素颜");
        assert_eq!(response.songs[1].artists[0].id, "000CK5xN3yZDJt");
        assert_eq!(response.songs[2].id, "00264YJC1bmylC");
        assert_eq!(response.songs[2].album.id, "000aM5Ia10a5d8");
        assert_eq!(response.songs[2].album.name, "2017安徽卫视上星20周年");
        assert_eq!(response.songs[2].artists[0].id, "000CK5xN3yZDJt");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_detail_artist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .artist()
            .id("3684")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.songs[0].id, "108485");
        assert_eq!(response.songs[0].album.id, "10770");
        assert_eq!(response.songs[0].album.name, "JJ陆");
        assert_eq!(response.name, "林俊杰");
        assert!(response.description.starts_with("JJ林俊杰的创作来自最深的情感"))
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_detail_artist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .artist()
            .id("001BLpXF2DyJe2")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.songs[0].id, "001N8e5Q4Gjxda");
        assert_eq!(response.songs[0].album.id, "002g6zv02X7SNi");
        assert_eq!(response.songs[0].album.name, "JJ陆");
        assert_eq!(response.name, "林俊杰");
        assert!(response.description.starts_with("JJ林俊杰的创作来自最深的情感"))
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_detail_album_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .album()
            .id("10770")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.songs[0].id, "108458");
        assert_eq!(response.songs[0].name, "SIXOLOGY");
        assert_eq!(response.name, "JJ陆");
        assert!(response.description.starts_with("JJ林俊杰 2008最新创作大碟 JJ陆"));
        assert!(!response.pic_url.is_empty())
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_detail_album_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .album()
            .id("002g6zv02X7SNi")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.songs[0].id, "002vb5JD04eGw3");
        assert_eq!(response.songs[0].name, "Sixology");
        assert_eq!(response.name, "JJ陆");
        assert!(response.description.starts_with("JJ林俊杰 2008最新创作大碟 JJ陆"));
        assert!(!response.pic_url.is_empty())
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_detail_playlist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .playlist()
            .id(2098357962)
            .platform(Platform::Netease)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.songs[0].id, "299601");
        assert_eq!(response.songs[0].name, "笑忘书");
        assert_eq!(response.name, "失败总是贯穿人生");
        assert!(response.description.starts_with("相逢如骤雨初晴"));
        assert!(!response.pic_url.is_empty())
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_detail_playlist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .playlist()
            .id(1790504159)
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.songs[0].id, "004TXEXY2G2c7C");
        assert_eq!(response.songs[0].name, "江南");
        assert_eq!(response.name, "青春记忆 | 90后校园岁月的流行歌曲");
        assert!(response.description.starts_with("教室里的不再是我们了"));
        assert!(!response.pic_url.is_empty())
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_detail_toplist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .toplist()
            .id(19723756)
            .platform(Platform::Netease)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.name, "飙升榜");
        assert_eq!(response.description, "云音乐中每天热度上升最快的100首单曲，每日更新。");
        assert!(!response.pic_url.is_empty())
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_detail_toplist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .detail()
            .toplist()
            .id(62)
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("detail should succeed");
        assert_eq!(response.name, "飙升榜");
        assert!(response.description.starts_with("1. 榜单定义：QQ音乐站内播"));
        assert!(!response.pic_url.is_empty())
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_playback_url_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .playback()
            .url()
            .id("108914")
            .level(SongQuality::Master)
            .platform(Platform::Netease)
            .send()
            .await
            .expect("playback should succeed");
        let parsed = reqwest::Url::parse(&response.url).expect("url should be valid");
        let host = parsed.host_str().expect("host should exist");
        assert!(host.ends_with(".music.126.net"), "unexpected host: {host}");
        assert_eq!(response.level, SongQuality::Standard);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_playback_url_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .playback()
            .url()
            .id("004Gq0xE1YC8xp")
            .level(SongQuality::Master)
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("playback should succeed");
        assert!(response.url.starts_with("https://isure.stream.qqmusic.qq.com/"));
        assert_eq!(response.level, SongQuality::Standard);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_playback_lyric_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .playback()
            .lyric()
            .id("3362722900")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("playback should succeed");
        assert!(response.lyric.starts_with("[00:05.000]纯音乐，请欣赏"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_playback_lyric_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .playback()
            .lyric()
            .id("004Gq0xE1YC8xp")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("playback should succeed");
        assert!(response.lyric.starts_with("[00:00.270]素颜 - 许嵩/何曼婷"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_discover_hotkey_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .hotkey()
            .platform(Platform::Netease)
            .send()
            .await
            .expect("discover should succeed");
        assert_eq!(response.hotkey.len(), 20);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_discover_hotkey_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .hotkey()
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("discover should succeed");
        assert_eq!(response.hotkey.len(), 20);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_discover_recommend_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .recommend_playlist()
            .platform(Platform::Netease)
            .send()
            .await
            .expect("discover should succeed");
        assert_eq!(response.playlists.len(), 6);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_discover_recommend_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .recommend_playlist()
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("discover should succeed");
        assert_eq!(response.playlists.len(), 6);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_discover_toplist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .toplist_list()
            .platform(Platform::Netease)
            .send()
            .await
            .expect("discover should succeed");
        assert_eq!(response.toplists[0].name, "飙升榜");
        assert_eq!(response.toplists[7].name, "网易云中文说唱榜");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_discover_toplist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .toplist_list()
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("discover should succeed");
        assert_eq!(response.toplists[0].name, "飙升榜");
        assert_eq!(response.toplists[1].name, "热歌榜");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_discover_categories_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .playlist_categories()
            .platform(Platform::Netease)
            .send()
            .await
            .expect("discover should succeed");
        assert_eq!(response.categories[0].name, "全部歌单");
        assert_eq!(response.categories[1].name, "综艺");
        assert_eq!(response.categories[0].id, "全部歌单");
        assert_eq!(response.categories[1].id, "综艺");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_discover_categories_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .playlist_categories()
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("discover should succeed");
        assert_eq!(response.categories[0].name, "官方歌单");
        assert_eq!(response.categories[1].name, "私藏");
        assert_eq!(response.categories[0].id, "3317");
        assert_eq!(response.categories[1].id, "3417");
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_discover_playlist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .playlist_list()
            .category("全部歌单")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("discover should succeed");
        assert!(response.more);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_discover_playlist_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .playlist_list()
            .category("3317")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("discover should succeed");
        assert!(response.more);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn netease_discover_suggests_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .suggests()
            .keyword("周杰伦")
            .platform(Platform::Netease)
            .send()
            .await
            .expect("discover should succeed");
        assert!(response.suggests[0].starts_with("周杰伦"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires live network"]
    async fn tencent_discover_suggests_returns_correct_response() {
        let client = MusicClient::new();
        let response = client
            .discover()
            .suggests()
            .keyword("周杰伦")
            .platform(Platform::Tencent)
            .send()
            .await
            .expect("discover should succeed");
        assert!(response.suggests[0].starts_with("周杰伦"));
    }
}

mod section_live_level_url_with_env_login {
    use std::env;

    use super::*;
    use crate::models::{NeteaseLoginToken, TencentLoginToken};

    fn load_dotenv() {
        let _ = dotenvy::dotenv();
    }

    fn required_env(name: &str) -> String {
        env::var(name).unwrap_or_else(|_| {
            panic!("missing env `{name}` in .env");
        })
    }

    fn env_with_default(name: &str, default_value: &str) -> String {
        match env::var(name) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => default_value.to_owned(),
        }
    }

    fn optional_i64_env(name: &str) -> Option<i64> {
        match env::var(name) {
            Ok(raw) if !raw.trim().is_empty() => Some(raw.parse::<i64>().unwrap_or_else(|_| {
                panic!("env `{name}` should be i64, got `{raw}`");
            })),
            _ => None,
        }
    }

    fn required_u64_env(name: &str) -> u64 {
        let raw = required_env(name);
        raw.parse::<u64>().unwrap_or_else(|_| panic!("env `{name}` should be u64, got `{raw}`"))
    }

    fn netease_token_from_env() -> NeteaseLoginToken {
        NeteaseLoginToken::new(
            required_env("NETEASE_MUSIC_U"),
            required_env("NETEASE_MUSIC_R_U"),
            required_env("NETEASE_CSRF"),
            optional_i64_env("NETEASE_EXPIRES_AT"),
        )
    }

    fn tencent_token_from_env() -> TencentLoginToken {
        TencentLoginToken::new(
            required_u64_env("TENCENT_MUSIC_ID"),
            required_env("TENCENT_MUSIC_KEY"),
            required_env("TENCENT_REFRESH_TOKEN"),
            required_env("TENCENT_REFRESH_KEY"),
            optional_i64_env("TENCENT_EXPIRES_AT"),
            required_u64_env("TENCENT_LOGIN_TYPE"),
        )
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires .env credentials and live network"]
    async fn netease_playback_url_accepts_multiple_levels_with_env_login() {
        load_dotenv();
        let client = MusicClient::new();
        let token = netease_token_from_env();
        let song_id = env_with_default("NETEASE_TEST_SONG_ID", "1859245776");
        let levels = [
            SongQuality::Standard,
            SongQuality::Exhigh,
            SongQuality::Lossless,
            SongQuality::Hires,
            SongQuality::Stereo,
            SongQuality::Surround,
            SongQuality::Master,
        ];

        for level in levels {
            let response = client
                .playback()
                .url()
                .id(song_id.as_str())
                .level(level.clone())
                .platform(Platform::Netease)
                .login(&token)
                .send()
                .await
                .unwrap_or_else(|error| panic!("netease level {level:?} request failed: {error}"));
            let parsed = reqwest::Url::parse(&response.url)
                .unwrap_or_else(|_| panic!("netease level {level:?} returned invalid url"));
            let host = parsed
                .host_str()
                .unwrap_or_else(|| panic!("netease level {level:?} returned url without host"));
            assert!(host.ends_with(".music.126.net"), "unexpected netease host: {host}");
            assert_eq!(response.level, level, "unexpected level for netease level {level:?}");
            assert_eq!(response.id, song_id, "unexpected id for netease level {level:?}");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "requires .env credentials and live network"]
    async fn tencent_playback_url_accepts_multiple_levels_with_env_login() {
        load_dotenv();
        let client = MusicClient::new();
        let token = tencent_token_from_env();
        let song_id = env_with_default("TENCENT_TEST_SONG_ID", "004Gq0xE1YC8xp");
        let levels = [
            SongQuality::Standard,
            SongQuality::Exhigh,
            SongQuality::Lossless,
            SongQuality::Hires,
            SongQuality::Stereo,
            SongQuality::Surround,
            SongQuality::Master,
        ];

        for level in levels {
            let response = client
                .playback()
                .url()
                .id(song_id.as_str())
                .level(level.clone())
                .platform(Platform::Tencent)
                .login(&token)
                .send()
                .await
                .unwrap_or_else(|error| panic!("tencent level {level:?} request failed: {error}"));
            let parsed = reqwest::Url::parse(&response.url)
                .unwrap_or_else(|_| panic!("tencent level {level:?} returned invalid url"));
            let host = parsed
                .host_str()
                .unwrap_or_else(|| panic!("tencent level {level:?} returned url without host"));
            assert!(host.ends_with("qqmusic.qq.com"), "unexpected tencent host: {host}");
            assert_eq!(response.id, song_id, "unexpected id for tencent level {level:?}");
        }
    }
}
