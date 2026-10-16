use anyhow::{Context as _, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{ProtocolClient, library::parse_track};
use crate::{Chart, ChartGroup, CredentialSession, PlaylistPage};

#[derive(Deserialize)]
struct ChartDirectory {
    group: Vec<ChartGroup>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ChartDetail {
    data: Chart,
    song_info_list: Vec<Value>,
}

impl ProtocolClient {
    pub async fn chart_groups(&self, credential: &CredentialSession) -> Result<Vec<ChartGroup>> {
        let data = self
            .call(
                "music.musicToplist.Toplist",
                "GetAll",
                json!({}),
                credential,
                None,
            )
            .await
            .context("无法加载 QQ 音乐排行榜")?;
        let directory: ChartDirectory =
            serde_json::from_value(data).context("QQ 音乐排行榜目录格式发生了变化")?;
        Ok(directory.group)
    }

    pub(super) async fn chart_page(
        &self,
        credential: &CredentialSession,
        top_id: u64,
        period: &str,
        offset: u64,
        limit: u64,
    ) -> Result<PlaylistPage> {
        let data = self
            .call(
                "music.musicToplist.Toplist",
                "GetDetail",
                json!({"topId": top_id, "period": period, "offset": offset, "num": limit}),
                credential,
                None,
            )
            .await
            .context("无法加载 QQ 音乐榜单歌曲")?;
        parse_chart_page(data, top_id, period, offset)
    }
}

fn parse_chart_page(data: Value, top_id: u64, period: &str, offset: u64) -> Result<PlaylistPage> {
    let detail: ChartDetail =
        serde_json::from_value(data).context("QQ 音乐榜单详情格式发生了变化")?;
    ensure!(
        detail.data.top_id == top_id && detail.data.period == period,
        "QQ 音乐返回的榜单或期数与请求不符"
    );
    let tracks = detail
        .song_info_list
        .iter()
        .map(parse_track)
        .collect::<Result<Vec<_>>>()?;
    let next_offset = offset + tracks.len() as u64;
    let total = detail.data.total_num;
    Ok(PlaylistPage {
        playlist: detail.data.playlist(period.to_owned()),
        has_more: !tracks.is_empty() && next_offset < total,
        tracks,
        total,
        next_offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chart_period_and_page() {
        let data = json!({
            "data": {
                "topId": 5, "title": "内地榜", "period": "2026_41",
                "frontPicUrl": "https://example.com/chart.jpg", "updateTips": "每周更新",
                "totalNum": 100, "song": [],
                "history": {"year": [2026, 2025], "subPeriod": [[42, 41], [52, 50]]}
            },
            "songInfoList": [{"id": 12, "mid": "song-mid", "title": "歌曲", "interval": 180}]
        });
        let page = parse_chart_page(data.clone(), 5, "2026_41", 50).unwrap();
        assert_eq!(page.next_offset, 51);
        assert!(page.has_more);
        assert_eq!(page.tracks[0].mid, "song-mid");
        assert_eq!(
            page.playlist.id,
            crate::UserPlaylistId::Chart {
                top_id: 5,
                period: "2026_41".to_owned(),
            }
        );
        assert!(parse_chart_page(data.clone(), 5, "2026_42", 0).is_err());
        assert!(parse_chart_page(data.clone(), 26, "2026_41", 0).is_err());
        let last = parse_chart_page(data, 5, "2026_41", 99).unwrap();
        assert!(!last.has_more);
    }
}
