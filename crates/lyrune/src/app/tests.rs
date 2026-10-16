use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use gpui::{Pixels, px};
use lyrune_api::{
    PlaylistPage, Quality, RecommendationKind, SearchAlbum, SearchPage, SearchResults, Track,
    UserPlaylist, UserPlaylistId,
};

use super::animation::frame_is_due;
use super::home::HomePageState;
use super::lyrics::parse_lyrics;
use super::lyrics_view::{
    adjacent_lyric_timing, combined_lyric_frame_interval, lyric_edge_opacity,
    lyric_horizontal_scroll_offset, lyric_position_for_frame_rate,
};
use super::navigation::NavigationHistory;
use super::navigation::{
    ArtistResource, NavigationPage, PageResourceCache, PlaylistCachePolicy, PlaylistResource,
    PlaylistScrollPosition, SearchCategory, SearchMoreResults, SearchResource, SearchVisibleCounts,
    resolved_playlist_scroll_row,
};
use super::playback::{PlaybackLoad, PlaybackState};
use super::playlists::playlist_title_is_long;
use super::progress_view::format_playback_time;
use super::queue::{
    PlaybackQueue, canonical_queue_track_index, insert_external_track_after_current,
    insert_track_after_current,
};
use crate::settings::{DEFAULT_NAVIGATION_HISTORY_LIMIT, FrameRate, PersistedQueueContinuation};

fn track(mid: &str) -> Track {
    Track {
        song_id: None,
        song_type: 0,
        mid: mid.to_owned(),
        media_mid: None,
        standard_size_bytes: None,
        high_size_bytes: None,
        lossless_size_bytes: None,
        hi_res_size_bytes: None,
        atmos_stereo_size_bytes: None,
        atmos_surround_size_bytes: None,
        master_size_bytes: None,
        title: mid.to_owned(),
        artists: String::new(),
        artist_details: Vec::new(),
        album: String::new(),
        album_mid: String::new(),
        cover_url: None,
        duration_seconds: 180,
    }
}

fn mids(tracks: &[Arc<Track>]) -> Vec<&str> {
    tracks.iter().map(|track| track.mid.as_str()).collect()
}

#[test]
fn test_home_recommendation_request() {
    let mut home = HomePageState::default();
    let kind = RecommendationKind::Radar;
    let old = home.begin_recommendation(kind);

    // 退出账号、换歌单或搜索选曲都通过清空加载状态使首批推荐失效。
    home.recommendation_loading = None;
    assert!(!home.finish_recommendation(old, kind));

    let current = home.begin_recommendation(kind);
    assert!(!home.finish_recommendation(old, kind));
    assert_eq!(home.recommendation_loading, Some(kind));
    assert!(home.finish_recommendation(current, kind));
    assert_eq!(home.recommendation_loading, None);
}

#[test]
fn test_replace_playback_queue() {
    let queue = |mid: &str| PlaybackQueue {
        playlist_id: UserPlaylistId::Search {
            query: mid.to_owned(),
        },
        tracks: vec![Arc::new(track(mid))],
        modified: false,
        continuation: None,
    };
    let mut playback = PlaybackState::new(None, Quality::Standard, None);
    let first = playback.replace_queue(queue("first"));
    playback.recommendation_loading = true;
    playback.waiting_for_recommendation = true;
    playback.current_track = Some(0);
    playback.position = Duration::from_secs(42);
    let audio_request = playback.load.invalidate();
    playback.load.track = Some(0);
    playback.load.autoplay = true;

    let replacement = playback.replace_queue(queue("replacement"));
    assert_ne!(replacement, first);
    assert!(!playback.recommendation_loading);
    assert!(!playback.waiting_for_recommendation);
    assert_eq!(
        playback.track().map(|track| track.mid.as_str()),
        Some("replacement")
    );
    // 换队列只结束旧补页/推荐周期；音源失效和停止仍由播放入口按原顺序完成。
    assert!(playback.load.is_current(audio_request));
    assert_eq!(playback.load.track, Some(0));
    assert!(playback.load.autoplay);
    assert_eq!(playback.position, Duration::from_secs(42));
}

#[test]
fn test_playback_snapshot() {
    let mut playback = PlaybackState::new(None, Quality::High, None);
    playback.position = Duration::from_micros(123_456);
    assert!(playback.snapshot(123).is_none());
    playback.queue = Some(PlaybackQueue {
        playlist_id: UserPlaylistId::Search {
            query: "saved".to_owned(),
        },
        tracks: vec![Arc::new(track("first")), Arc::new(track("second"))],
        modified: true,
        continuation: Some(PersistedQueueContinuation::Guess),
    });
    assert!(playback.snapshot(123).is_none());
    playback.current_track = Some(2);
    assert!(playback.snapshot(123).is_none());
    playback.current_track = Some(1);
    playback.load.track = Some(1);

    let snapshot = playback.snapshot(123).expect("valid selected queue track");
    assert_eq!(snapshot.account_id, 123);
    assert_eq!(
        snapshot.playlist_id,
        UserPlaylistId::Search {
            query: "saved".to_owned()
        }
    );
    assert_eq!(snapshot.track_mid, "second");
    assert_eq!(snapshot.position_ms, 123);
    assert_eq!(mids(&snapshot.queue_tracks), vec!["first", "second"]);
    assert!(snapshot.queue_modified);
    assert_eq!(
        snapshot.queue_continuation,
        Some(PersistedQueueContinuation::Guess)
    );
    assert_eq!(playback.position, Duration::from_micros(123_456));
}

#[test]
fn playback_load_preserves_intent_until_cleared() {
    for autoplay in [true, false] {
        let mut load = PlaybackLoad::default();
        let original = load.invalidate();
        load.track = Some(2);
        load.autoplay = autoplay;

        // 同一首歌在加载中切换音质，索引未变也必须拒绝旧结果。
        let replacement = load.invalidate();
        assert!(!load.is_current(original));
        assert!(load.is_current(replacement));
        assert_eq!(load.track, Some(2));
        assert_eq!(load.autoplay, autoplay);

        load.clear();
        assert_eq!(load.track, None);
        assert!(!load.autoplay);
        assert!(!load.is_current(original));
        assert!(load.is_current(replacement));
    }
}

#[test]
fn playback_load_rejects_requests_across_restarts() {
    let mut load = PlaybackLoad::default();
    let original = load.invalidate();
    load.track = Some(0);
    load.autoplay = true;

    // 停止或退出先使请求失效，之后清理；下一次开始不能复用旧版本。
    load.invalidate();
    load.clear();
    assert!(!load.is_current(original));

    let restarted = load.invalidate();
    load.track = Some(0);
    assert!(!load.is_current(original));
    assert!(load.is_current(restarted));
    assert!(!load.autoplay);
}

#[test]
fn horizontal_lyric_scroll_starts_at_the_anchor_and_stops_at_the_end() {
    assert_eq!(
        lyric_horizontal_scroll_offset(px(500.), px(600.), px(500.)),
        px(0.)
    );
    assert_eq!(
        lyric_horizontal_scroll_offset(px(1000.), px(600.), px(250.)),
        px(0.)
    );
    assert_eq!(
        lyric_horizontal_scroll_offset(px(1000.), px(600.), px(300.)),
        px(48.)
    );
    assert_eq!(
        lyric_horizontal_scroll_offset(px(1000.), px(600.), px(1000.)),
        px(400.)
    );
}

#[test]
fn lyric_rows_fade_before_reaching_the_viewport_edge() {
    assert_eq!(lyric_edge_opacity(px(100.), px(204.), px(0.), px(400.)), 1.);
    assert!((lyric_edge_opacity(px(272.), px(376.), px(0.), px(400.)) - 0.5).abs() < 1e-6);
    assert_eq!(lyric_edge_opacity(px(296.), px(400.), px(0.), px(400.)), 0.);
    assert_eq!(lyric_edge_opacity(px(-10.), px(94.), px(0.), px(400.)), 0.);
}

#[test]
fn lyric_frame_intervals_preserve_the_fastest_animation_requirement() {
    assert_eq!(
        combined_lyric_frame_interval(FrameRate::Fps30, FrameRate::Fps60),
        FrameRate::Fps60.frame_interval()
    );
    assert_eq!(
        combined_lyric_frame_interval(FrameRate::Display, FrameRate::Fps60),
        None
    );
    assert_eq!(
        combined_lyric_frame_interval(FrameRate::Fps30, FrameRate::Display),
        None
    );
}

#[test]
fn lyric_position_is_quantized_only_for_limited_highlight_rates() {
    let position = Duration::from_millis(50);
    assert_eq!(
        lyric_position_for_frame_rate(position, FrameRate::Fps30),
        Duration::from_nanos(1_000_000_000 / 30)
    );
    assert_eq!(
        lyric_position_for_frame_rate(position, FrameRate::Display),
        position
    );
}

#[test]
fn limited_frames_follow_one_stable_deadline() {
    let now = Instant::now();
    let interval = FrameRate::Fps30.frame_interval().unwrap();
    let mut next_frame = None;

    assert!(!frame_is_due(now, FrameRate::Fps30, &mut next_frame));
    assert_eq!(next_frame, Some(now + interval));
    assert!(!frame_is_due(
        now + Duration::from_millis(10),
        FrameRate::Fps30,
        &mut next_frame
    ));
    assert!(frame_is_due(
        now + interval,
        FrameRate::Fps30,
        &mut next_frame
    ));
    assert!(frame_is_due(
        now + interval,
        FrameRate::Display,
        &mut next_frame
    ));
    assert_eq!(next_frame, None);
}

#[test]
fn untimed_qrc_tail_extends_to_the_next_line() {
    let timing = (Duration::from_millis(100), Duration::from_millis(600));

    assert_eq!(
        adjacent_lyric_timing(
            Some(timing),
            None,
            Duration::ZERO,
            Duration::from_millis(900)
        ),
        (Duration::from_millis(600), Duration::from_millis(900))
    );
}

#[test]
fn untimed_qrc_text_between_words_uses_the_timing_gap() {
    let previous = (Duration::from_millis(100), Duration::from_millis(600));
    let next = (Duration::from_millis(900), Duration::from_millis(1_200));

    assert_eq!(
        adjacent_lyric_timing(
            Some(previous),
            Some(next),
            Duration::ZERO,
            Duration::from_millis(1_500)
        ),
        (Duration::from_millis(600), Duration::from_millis(900))
    );
}

#[test]
fn prefers_qrc_word_timing_and_preserves_lyric_parentheses() {
    let lyrics = parse_lyrics(
        r#"<?xml version="1.0" encoding="utf-8"?>
<QrcInfos><LyricInfo LyricCount="1"><Lyric_1 LyricType="1" LyricContent="[1000,700]青(1000,300)い(1300,150)空（そら）(1450,250)&#10;[2000,500]次(2000,500)"/></LyricInfo></QrcInfos>"#,
        Some("[00:01.000]蓝色天空\n[00:02.000]下一句"),
        None,
    );

    assert_eq!(lyrics.lines.len(), 2);
    assert_eq!(lyrics.lines[0].start, Duration::from_secs(1));
    assert_eq!(lyrics.lines[0].text, "青い空（そら）");
    assert_eq!(lyrics.lines[0].words.len(), 3);
    assert_eq!(
        lyrics.lines[0].words[0].highlight_progress(Duration::from_millis(999)),
        0.
    );
    assert!(
        (lyrics.lines[0].words[0].highlight_progress(Duration::from_millis(1_100)) - 1. / 3.).abs()
            < f32::EPSILON
    );
    assert_eq!(
        lyrics.lines[0].words[2].highlight_progress(Duration::from_millis(1_700)),
        1.
    );
    assert_eq!(lyrics.lines[0].translation.as_deref(), Some("蓝色天空"));
    assert_eq!(lyrics.active_index(Duration::from_millis(1_999)), Some(0));
    assert_eq!(lyrics.active_index(Duration::from_secs(2)), Some(1));
}

fn playlist(diss_id: u64) -> NavigationPage {
    playlist_at_scroll_row(diss_id, 0)
}

fn playlist_at_scroll_row(diss_id: u64, scroll_row: usize) -> NavigationPage {
    playlist_at_scroll_position(diss_id, scroll_row, px(0.))
}

fn playlist_at_scroll_position(
    diss_id: u64,
    scroll_row: usize,
    offset_y: Pixels,
) -> NavigationPage {
    NavigationPage::Playlist {
        playlist: UserPlaylist {
            id: UserPlaylistId::Favorite { diss_id },
            title: format!("playlist-{diss_id}"),
            cover_url: None,
            description: String::new(),
            owner: String::new(),
            owner_avatar_url: None,
            track_count: 0,
        },
        selected_index: None,
        scroll_position: PlaylistScrollPosition {
            row: scroll_row,
            offset_y,
        },
        resource: None,
    }
}

#[test]
fn test_page_resource_lifetime() {
    let mut cache = PageResourceCache::default();
    let search = cache.search(7, "same query");
    assert!(Arc::ptr_eq(&search, &cache.search(7, "same query")));
    assert!(!Arc::ptr_eq(&search, &cache.search(8, "same query")));
    let artist = cache.artist(7, "artist");
    assert!(Arc::ptr_eq(&artist, &cache.artist(7, "artist")));
    assert!(!Arc::ptr_eq(&artist, &cache.artist(8, "artist")));
    let weak = Arc::downgrade(&search);
    drop(search);
    cache.prune();
    assert!(weak.upgrade().is_none());
    assert!(Arc::ptr_eq(&artist, &cache.artist(7, "artist")));

    let NavigationPage::Playlist { playlist, .. } = playlist(1) else {
        unreachable!();
    };
    let stale = cache.playlist(7, playlist.clone(), false, PlaylistCachePolicy::Fresh);
    assert!(Arc::ptr_eq(
        &stale,
        &cache.playlist(7, playlist.clone(), false, PlaylistCachePolicy::AllowStale)
    ));
    let fresh = cache.playlist(7, playlist.clone(), false, PlaylistCachePolicy::Fresh);
    assert!(!Arc::ptr_eq(&stale, &fresh));
    fresh.lock().unwrap().fetched_at_secs = super::unix_timestamp_secs();
    assert!(Arc::ptr_eq(
        &fresh,
        &cache.playlist(7, playlist.clone(), false, PlaylistCachePolicy::Fresh)
    ));
    let forced = cache.playlist(7, playlist.clone(), true, PlaylistCachePolicy::AllowStale);
    assert!(!Arc::ptr_eq(&fresh, &forced));
    assert!(Arc::ptr_eq(
        &forced,
        &cache.cached_playlist(7, &playlist.id).unwrap()
    ));
    assert!(cache.cached_playlist(8, &playlist.id).is_none());
    // 更新索引不会清空历史页仍持有的旧资源。
    assert_eq!(fresh.lock().unwrap().playlist.id, playlist.id);
    let weak = Arc::downgrade(&forced);
    drop(forced);
    cache.prune();
    assert!(weak.upgrade().is_none());
    assert!(cache.cached_playlist(7, &playlist.id).is_none());
}

#[test]
fn new_navigation_after_back_clears_the_forward_branch() {
    let first = playlist(1);
    let second = playlist(2);
    let home = NavigationPage::Home;
    let mut history = NavigationHistory::default();

    history.record(Some(first.clone()), &home);
    let back = history.go_back(Some(home.clone())).unwrap();
    assert!(back.same_destination(&first));

    let forward = history.go_forward(Some(first.clone())).unwrap();
    assert!(forward.same_destination(&home));

    let back = history.go_back(Some(home)).unwrap();
    assert!(back.same_destination(&first));
    history.record(Some(first), &second);

    assert!(history.forward.is_empty());
    let back = history.go_back(Some(second)).unwrap();
    assert!(back.same_destination(&playlist(1)));
}

#[test]
fn new_navigation_can_claim_a_forward_resource_before_clearing_it() {
    let NavigationPage::Playlist { playlist, .. } = playlist(1) else {
        unreachable!();
    };
    let resource = Arc::new(std::sync::Mutex::new(PlaylistResource::empty(
        playlist.clone(),
    )));
    let weak = Arc::downgrade(&resource);
    let mut history = NavigationHistory {
        limit: DEFAULT_NAVIGATION_HISTORY_LIMIT,
        back: Vec::new(),
        forward: vec![NavigationPage::Playlist {
            playlist: playlist.clone(),
            selected_index: None,
            scroll_position: PlaylistScrollPosition::top(),
            resource: Some(resource),
        }],
    };

    let claimed = weak.upgrade().expect("forward resource remains available");
    let target = NavigationPage::Playlist {
        playlist,
        selected_index: None,
        scroll_position: PlaylistScrollPosition::top(),
        resource: Some(claimed),
    };
    history.record(Some(NavigationPage::Home), &target);

    assert!(history.forward.is_empty());
    assert!(weak.upgrade().is_some());
}

#[test]
fn navigation_history_keeps_at_most_ten_pages_including_the_current_page() {
    let mut history = NavigationHistory::default();
    let mut current = NavigationPage::Home;
    for diss_id in 1..=DEFAULT_NAVIGATION_HISTORY_LIMIT as u64 + 2 {
        let target = playlist(diss_id);
        history.record(Some(current), &target);
        current = target;
    }

    assert_eq!(history.back.len() + 1, DEFAULT_NAVIGATION_HISTORY_LIMIT);
    assert!(history.back[0].same_destination(&playlist(3)));
}

#[test]
fn shrinking_navigation_history_limit_evicts_the_oldest_pages_immediately() {
    let mut history = NavigationHistory::new(4);
    let mut current = NavigationPage::Home;
    for diss_id in 1..=3 {
        let target = playlist(diss_id);
        history.record(Some(current), &target);
        current = target;
    }

    history.set_limit(2);

    assert_eq!(history.back.len() + history.forward.len() + 1, 2);
    assert!(history.back[0].same_destination(&playlist(2)));
}

#[test]
fn evicting_history_releases_its_shared_page_resource() {
    let resource = Arc::new(std::sync::Mutex::new(SearchResource::default()));
    let weak = Arc::downgrade(&resource);
    let mut current = NavigationPage::Search {
        query: "resource lifetime".to_owned(),
        category: SearchCategory::Songs,
        visible_counts: SearchVisibleCounts::default(),
        scroll_offsets: [px(0.); 4],
        resource: Some(resource.clone()),
    };
    drop(resource);

    let mut history = NavigationHistory::default();
    for diss_id in 1..=DEFAULT_NAVIGATION_HISTORY_LIMIT as u64 {
        let target = playlist(diss_id);
        history.record(Some(current), &target);
        current = target;
    }

    assert!(weak.upgrade().is_none());
}

#[test]
fn playlist_history_keeps_its_exact_scroll_position() {
    let playlist = playlist_at_scroll_position(1, 37, px(-18.));
    let mut history = NavigationHistory::default();

    history.record(Some(playlist), &NavigationPage::Home);
    let restored = history
        .go_back(Some(NavigationPage::Home))
        .expect("playlist history entry");

    let NavigationPage::Playlist {
        scroll_position, ..
    } = restored
    else {
        panic!("restored playlist history entry");
    };
    assert_eq!(scroll_position.row, 37);
    assert_eq!(scroll_position.offset_y, px(-18.));
}

#[test]
fn playlist_scroll_waits_until_the_requested_row_is_loaded() {
    assert_eq!(resolved_playlist_scroll_row(120, 100, true), None);
    assert_eq!(resolved_playlist_scroll_row(120, 200, true), Some(120));
    assert_eq!(resolved_playlist_scroll_row(250, 200, false), Some(199));
}

#[test]
fn shared_playlist_ignores_a_late_duplicate_page() {
    let NavigationPage::Playlist { playlist, .. } = playlist(1) else {
        unreachable!();
    };
    let mut resource = PlaylistResource::empty(playlist.clone());
    resource.apply_page(
        playlist.clone(),
        vec![Arc::new(track("first"))],
        true,
        20,
        0,
    );
    resource.apply_page(
        playlist.clone(),
        vec![Arc::new(track("second"))],
        true,
        40,
        20,
    );
    resource.apply_page(playlist, vec![Arc::new(track("duplicate"))], false, 40, 20);

    assert_eq!(mids(&resource.tracks), ["first", "second"]);
    assert!(resource.has_more);
    assert_eq!(resource.next_offset, 40);
}

#[test]
fn test_chart_cover_preserved() {
    let mut playlist = UserPlaylist::liked();
    playlist.id = UserPlaylistId::Chart {
        top_id: 5,
        period: "2025_50".into(),
    };
    playlist.cover_url = Some("https://example.com/chart.jpg".into());
    let mut resource = PlaylistResource::empty(playlist.clone());
    playlist.cover_url = Some("https://example.com/2025_50.jpg".into());
    playlist.track_count = 2;

    for offset in 0..2 {
        resource.apply_page(
            playlist.clone(),
            vec![Arc::new(track(&offset.to_string()))],
            offset == 0,
            offset + 1,
            offset,
        );
        assert_eq!(
            resource.playlist.cover_url.as_deref(),
            Some("https://example.com/chart.jpg")
        );
    }
    assert_eq!(resource.playlist.id, playlist.id);
    assert_eq!(resource.playlist.track_count, 2);
    assert_eq!(mids(&resource.tracks), ["0", "1"]);
}

#[test]
fn playback_time_uses_compact_player_formatting() {
    assert_eq!(format_playback_time(Duration::from_secs(137)), "2:17");
    assert_eq!(format_playback_time(Duration::from_secs(3_661)), "1:01:01");
}

#[test]
fn playlist_title_length_accounts_for_wide_characters() {
    assert!(!playlist_title_is_long("123456789012345678901234"));
    assert!(playlist_title_is_long("1234567890123456789012345"));
    assert!(!playlist_title_is_long("一二三四五六七八九十一二"));
    assert!(playlist_title_is_long("一二三四五六七八九十一二三"));
}

#[test]
fn search_history_keeps_the_active_category_without_splitting_one_query() {
    let visible_counts = SearchVisibleCounts {
        albums: 60,
        ..Default::default()
    };
    let songs = NavigationPage::Search {
        query: "周杰伦".to_owned(),
        category: SearchCategory::Songs,
        visible_counts: SearchVisibleCounts::default(),
        scroll_offsets: [px(0.); 4],
        resource: None,
    };
    let albums = NavigationPage::Search {
        query: "周杰伦".to_owned(),
        category: SearchCategory::Albums,
        visible_counts,
        scroll_offsets: [px(-120.), px(-360.), px(-720.), px(-180.)],
        resource: None,
    };
    assert!(songs.same_destination(&albums));

    let target = playlist(42);
    let mut history = NavigationHistory::default();
    history.record(Some(albums.clone()), &target);
    let restored = history.go_back(Some(target)).expect("search history entry");
    assert!(matches!(
        restored,
        NavigationPage::Search {
            query,
            category: SearchCategory::Albums,
            visible_counts,
            scroll_offsets,
            ..
        } if query == "周杰伦"
            && visible_counts.albums == 60
            && scroll_offsets == [px(-120.), px(-360.), px(-720.), px(-180.)]
    ));
}

#[test]
fn search_history_keeps_scroll_offsets_between_queries() {
    let previous = NavigationPage::Search {
        query: "milet".to_owned(),
        category: SearchCategory::Playlists,
        visible_counts: SearchVisibleCounts::default(),
        scroll_offsets: [px(-128.), px(-480.), px(-256.), px(-64.)],
        resource: None,
    };
    let next = NavigationPage::Search {
        query: "许嵩".to_owned(),
        category: SearchCategory::Songs,
        visible_counts: SearchVisibleCounts::default(),
        scroll_offsets: [px(0.); 4],
        resource: None,
    };
    let mut history = NavigationHistory::default();
    history.record(Some(previous), &next);
    let restored = history.go_back(Some(next)).expect("previous search");
    assert!(matches!(
        &restored,
        NavigationPage::Search { category: SearchCategory::Playlists, scroll_offsets, .. }
            if *scroll_offsets == [px(-128.), px(-480.), px(-256.), px(-64.)]
    ));
    let restored = history.go_forward(Some(restored)).expect("new search");
    assert!(matches!(
        restored,
        NavigationPage::Search { query, scroll_offsets, .. }
            if query == "许嵩" && scroll_offsets == [px(0.); 4]
    ));
}

#[test]
fn search_results_survive_paging_errors() {
    let mut resource = SearchResource {
        loading: true,
        ..Default::default()
    };
    resource.apply_results(Err(anyhow::anyhow!("offline")));
    assert!(!resource.loading && resource.results.is_none());
    assert_eq!(resource.error.as_deref(), Some("搜索失败：offline"));

    resource.loading = true;
    resource.apply_results(Ok(SearchResults {
        songs: SearchPage {
            items: vec![track("A")],
            has_more: true,
            next_offset: 1,
        },
        artists: SearchPage {
            items: vec![],
            has_more: false,
            next_offset: 0,
        },
        albums: SearchPage {
            items: vec![],
            has_more: true,
            next_offset: 0,
        },
        playlists: SearchPage {
            items: vec![],
            has_more: false,
            next_offset: 0,
        },
    }));
    assert!(!resource.loading && resource.error.is_none());

    resource.loading_more[SearchCategory::Songs.index()] = true;
    resource.loading_more[SearchCategory::Albums.index()] = true;
    resource.apply_more_results(
        SearchCategory::Albums,
        Err(anyhow::anyhow!("timeout").context("albums")),
    );
    assert!(!resource.loading_more[SearchCategory::Albums.index()]);
    assert!(resource.loading_more[SearchCategory::Songs.index()]);

    resource.apply_more_results(
        SearchCategory::Songs,
        Ok(SearchMoreResults::Songs(SearchPage {
            items: vec![track("B")],
            has_more: false,
            next_offset: 7,
        })),
    );
    let results = resource.results.as_ref().unwrap();
    assert_eq!(mids(&results.songs.items), ["A", "B"]);
    assert_eq!(results.songs.next_offset, 7);
    assert!(!results.songs.has_more);
    assert!(results.albums.items.is_empty() && results.albums.has_more);
    assert!(!resource.loading_more[SearchCategory::Songs.index()]);
    assert_eq!(
        resource.error.as_deref(),
        Some("继续加载搜索结果失败：albums: timeout")
    );
}

#[test]
fn artist_results_keep_independent_pages() {
    let album = SearchAlbum {
        mid: "album".to_owned(),
        title: "album".to_owned(),
        cover_url: None,
        artist: String::new(),
    };
    let first_page = PlaylistPage {
        playlist: UserPlaylist::liked(),
        tracks: vec![track("A")],
        total: 3,
        has_more: true,
        next_offset: 4,
    };
    let mut resource = ArtistResource {
        songs_loading: true,
        albums_loading: true,
        ..Default::default()
    };
    resource.apply_songs(Ok(first_page.clone()));
    assert!(!resource.songs_loading && resource.albums_loading);
    resource.apply_albums(
        0,
        Ok(SearchPage {
            items: vec![album.clone()],
            has_more: true,
            next_offset: 1,
        }),
    );
    assert!(!resource.albums_loading);

    resource.songs_loading = true;
    resource.albums_loading_more = true;
    resource.apply_songs(Err(anyhow::anyhow!("offline").context("songs")));
    let songs = resource.songs.as_ref().unwrap();
    assert_eq!(mids(&songs.items), ["A"]);
    assert_eq!(songs.next_offset, 4);
    assert!(songs.has_more);
    assert_eq!(resource.track_count, 3);
    assert!(!resource.songs_loading && resource.albums_loading_more);

    resource.apply_albums(
        1,
        Ok(SearchPage {
            items: vec![album],
            has_more: false,
            next_offset: 9,
        }),
    );
    let albums = resource.albums.as_ref().unwrap();
    assert_eq!(albums.items.len(), 2);
    assert_eq!(albums.next_offset, 9);
    assert!(!albums.has_more && !resource.albums_loading_more);
    assert_eq!(
        resource.song_error.as_deref(),
        Some("加载歌手歌曲失败：songs: offline")
    );

    resource.songs_loading = true;
    resource.song_error = None;
    resource.apply_songs(Ok(PlaylistPage {
        tracks: vec![track("B")],
        total: 6,
        has_more: false,
        next_offset: 9,
        ..first_page.clone()
    }));
    let songs = resource.songs.as_ref().unwrap();
    assert_eq!(mids(&songs.items), ["B"]);
    assert_eq!(songs.next_offset, 9);
    assert!(!songs.has_more && !resource.songs_loading);
    assert_eq!(resource.track_count, 6);

    resource.apply_songs(Ok(first_page));
    assert_eq!(mids(&resource.songs.as_ref().unwrap().items), ["A"]);
    assert_eq!(resource.albums.as_ref().unwrap().items.len(), 2);
}

#[test]
fn search_categories_follow_the_product_order() {
    assert_eq!(
        SearchCategory::ALL.map(SearchCategory::label),
        ["单曲", "歌单", "专辑", "歌手"]
    );
}

#[test]
fn search_track_is_inserted_after_the_current_track() {
    let mut tracks = vec![Arc::new(track("A")), Arc::new(track("B"))];

    let inserted = insert_track_after_current(&mut tracks, Some(0), Arc::new(track("C")));

    assert_eq!(inserted, 1);
    assert_eq!(mids(&tracks), ["A", "C", "B"]);
}

#[test]
fn existing_search_track_is_moved_without_duplication() {
    let mut tracks = vec![
        Arc::new(track("C")),
        Arc::new(track("A")),
        Arc::new(track("B")),
    ];

    let inserted = insert_track_after_current(&mut tracks, Some(1), Arc::new(track("C")));

    assert_eq!(inserted, 1);
    assert_eq!(mids(&tracks), ["A", "C", "B"]);
}

#[test]
fn queue_playback_markers_follow_page_order() {
    let queue = PlaybackQueue {
        playlist_id: UserPlaylistId::Liked,
        tracks: vec![Arc::new(track("A")), Arc::new(track("B"))],
        modified: false,
        continuation: None,
    };
    let reordered = vec![Arc::new(track("B")), Arc::new(track("A"))];
    assert_eq!(
        queue.playback_indices_in(&reordered, Some(0), Some(1)),
        (Some(1), Some(0))
    );

    let another_page = vec![Arc::new(track("C")), Arc::new(track("B"))];
    assert_eq!(
        queue.playback_indices_in(&another_page, Some(0), Some(1)),
        (None, Some(1))
    );
}

#[test]
fn queue_append_preserves_order_and_existing_tracks() {
    let mut queue = PlaybackQueue {
        playlist_id: UserPlaylistId::Liked,
        tracks: vec![Arc::new(track("A")), Arc::new(track("B"))],
        modified: false,
        continuation: None,
    };
    let mut duplicate = track("B");
    duplicate.title = "different metadata".to_owned();
    let mut repeated_new = track("C");
    repeated_new.title = "later duplicate".to_owned();

    let first_added = queue.append_tracks([
        Arc::new(duplicate),
        Arc::new(track("C")),
        Arc::new(repeated_new),
        Arc::new(track("D")),
    ]);

    assert_eq!(first_added, Some(2));
    assert_eq!(mids(&queue.tracks), ["A", "B", "C", "D"]);
    assert_eq!(queue.tracks[1].title, "B");
    assert_eq!(queue.tracks[2].title, "C");
    assert_eq!(
        canonical_queue_track_index(&queue, &UserPlaylistId::Liked, &queue.tracks, "D"),
        Some(3)
    );

    // 重复批次没有续播起点，也不能改变已有歌曲的顺序。
    assert_eq!(queue.append_tracks([Arc::new(track("C"))]), None);
    assert_eq!(queue.append_tracks([]), None);
    assert_eq!(mids(&queue.tracks), ["A", "B", "C", "D"]);
}

#[test]
fn search_insertion_prevents_reusing_the_source_playlist_queue() {
    let playlist_id = UserPlaylistId::Liked;
    let mut queue = PlaybackQueue {
        playlist_id: playlist_id.clone(),
        tracks: vec![Arc::new(track("A")), Arc::new(track("B"))],
        modified: false,
        continuation: None,
    };

    let inserted =
        insert_external_track_after_current(&mut queue, Some(0), Arc::new(track("search")));

    assert_eq!(inserted, 1);
    assert_eq!(mids(&queue.tracks), ["A", "search", "B"]);
    assert!(queue.modified);
    assert_eq!(
        canonical_queue_track_index(&queue, &playlist_id, &queue.tracks, "B"),
        None
    );
}

#[test]
fn test_daily_queue_refresh() {
    let queue = PlaybackQueue {
        playlist_id: UserPlaylistId::Daily,
        tracks: vec![Arc::new(track("A")), Arc::new(track("B"))],
        modified: false,
        continuation: None,
    };
    assert_eq!(
        canonical_queue_track_index(&queue, &UserPlaylistId::Daily, &queue.tracks, "B"),
        Some(1)
    );
    let today = vec![Arc::new(track("B")), Arc::new(track("C"))];
    assert_eq!(
        canonical_queue_track_index(&queue, &UserPlaylistId::Daily, &today, "B"),
        None
    );
}
