//! LRC/QRC 解析及翻译、注音对齐；不读取缓存，也不参与 GPUI 的排版和动画。

use std::ops::Range;
use std::time::Duration;

use gpui::SharedString;
use quick_xml::{Reader, escape::unescape, events::Event};
use wana_kana::{ConvertJapanese as _, IsJapaneseStr as _};

const TRANSLATION_ALIGNMENT_TOLERANCE: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LyricWord {
    // 范围以 UTF-8 字节计数，供原文切片和 GPUI TextRun 使用，不能当作字符下标。
    pub(super) range: Range<usize>,
    pub(super) start: Duration,
    pub(super) end: Duration,
    pub(super) ruby: Option<SharedString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct LyricLine {
    pub(super) start: Duration,
    pub(super) text: String,
    pub(super) words: Vec<LyricWord>,
    pub(super) translation: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct ParsedLyrics {
    pub(super) lines: Vec<LyricLine>,
}

impl ParsedLyrics {
    pub(super) fn active_index(&self, position: Duration) -> Option<usize> {
        self.lines
            .partition_point(|line| line.start <= position)
            .checked_sub(1)
    }
}

pub(super) fn parse_lyrics(
    original: &str,
    translated: Option<&str>,
    romanized: Option<&str>,
) -> ParsedLyrics {
    let mut original = parse_timed_lyrics(original);
    let translated = translated.map(parse_timed_lyrics).unwrap_or_default();
    if original.is_empty() {
        original = translated;
        return ParsedLyrics { lines: original };
    }

    align_lyric_lines(&mut original, &translated, |line, translation| {
        line.translation = provider_translation(&translation.text);
    });

    if original.iter().any(|line| {
        line.text
            .chars()
            .any(|character| matches!(character, '\u{3040}'..='\u{30ff}'))
    }) {
        let romanized = romanized.map(parse_timed_lyrics).unwrap_or_default();
        align_lyric_lines(&mut original, &romanized, attach_ruby);
    }

    ParsedLyrics { lines: original }
}

fn align_lyric_lines(
    original: &mut [LyricLine],
    auxiliary: &[LyricLine],
    mut merge: impl FnMut(&mut LyricLine, &LyricLine),
) {
    // 相同行数按顺序对齐，可容纳译文时间戳的取整偏差；行数不同时才做单向时间匹配。
    if original.len() == auxiliary.len() {
        for (line, auxiliary) in original.iter_mut().zip(auxiliary) {
            merge(line, auxiliary);
        }
        return;
    }

    let mut search_from = 0;
    for line in original {
        let Some((offset, auxiliary)) = auxiliary[search_from..]
            .iter()
            .enumerate()
            .take_while(|(_, auxiliary)| {
                auxiliary.start <= line.start.saturating_add(TRANSLATION_ALIGNMENT_TOLERANCE)
            })
            .filter(|(_, auxiliary)| {
                line.start.abs_diff(auxiliary.start) <= TRANSLATION_ALIGNMENT_TOLERANCE
            })
            .min_by_key(|(_, auxiliary)| line.start.abs_diff(auxiliary.start))
        else {
            continue;
        };
        merge(line, auxiliary);
        search_from += offset + 1;
    }
}

fn attach_ruby(line: &mut LyricLine, romanized: &LyricLine) {
    // 原文分词和罗马音分词并不一一对应，按最大时间重叠归属，再拼接成汉字注音。
    let mut readings = vec![String::new(); line.words.len()];
    let mut word_index = 0;
    let mut romanized_index = 0;
    let mut best_match = None;
    while word_index < line.words.len() && romanized_index < romanized.words.len() {
        let word = &line.words[word_index];
        let romanized_word = &romanized.words[romanized_index];
        let start = word.start.max(romanized_word.start);
        let end = word.end.min(romanized_word.end);
        if start < end {
            let overlap = end - start;
            match best_match {
                Some((_, best_overlap)) if best_overlap > overlap => {}
                _ => best_match = Some((word_index, overlap)),
            }
        }

        if romanized_word.end <= word.end {
            if let Some((matched_word_index, _)) = best_match.take() {
                readings[matched_word_index]
                    .push_str(&romanized.text[romanized_word.range.clone()]);
            }
            romanized_index += 1;
        }
        if word.end <= romanized_word.end {
            word_index += 1;
        }
    }
    if let (Some((matched_word_index, _)), Some(romanized_word)) =
        (best_match, romanized.words.get(romanized_index))
    {
        readings[matched_word_index].push_str(&romanized.text[romanized_word.range.clone()]);
    }

    for (word, reading) in line.words.iter_mut().zip(readings) {
        let text = &line.text[word.range.clone()];
        if !text.contains_kanji() {
            continue;
        }

        let reading = reading
            .chars()
            .filter(|character| !character.is_whitespace() && *character != '\'')
            .collect::<String>()
            .to_hiragana();
        if !reading.is_empty()
            && reading
                .chars()
                .all(|character| matches!(character, '\u{3040}'..='\u{309f}' | 'ー'))
        {
            word.ruby = Some(reading.into());
        }
    }
}

fn provider_translation(text: &str) -> Option<String> {
    (!text.trim().is_empty() && text.trim() != "//").then(|| text.to_owned())
}

fn parse_timed_lyrics(raw: &str) -> Vec<LyricLine> {
    let Some(content) = extract_qrc_content(raw) else {
        return parse_lrc(raw);
    };
    let qrc = parse_qrc(&content);
    if qrc.is_empty() {
        parse_lrc(&content)
    } else {
        qrc
    }
}

fn parse_qrc(content: &str) -> Vec<LyricLine> {
    let mut lines = content
        .lines()
        .filter_map(|raw_line| {
            let raw_line = raw_line.trim_start_matches('\u{feff}');
            let (start, consumed) = parse_qrc_line_timestamp(raw_line)?;
            let (text, words) = parse_qrc_words(&raw_line[consumed..]);
            (!text.is_empty()).then_some(LyricLine {
                start,
                text,
                words,
                translation: None,
            })
        })
        .collect::<Vec<_>>();
    lines.sort_by_key(|line| line.start);
    lines
}

fn extract_qrc_content(raw: &str) -> Option<String> {
    if !raw.trim_start().starts_with('<') {
        return Some(raw.to_owned());
    }

    // 部分 QRC 把未转义的引号、& 或 < 写进属性；宽松分支保留这些原文。
    // 严格与宽松路径都有回归样例，不能仅因存在 XML 解析器就删除后者。
    extract_qrc_content_strict(raw).or_else(|| extract_qrc_content_relaxed(raw))
}

fn extract_qrc_content_strict(raw: &str) -> Option<String> {
    let mut reader = Reader::from_str(raw);
    let mut lyric_content = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element) | Event::Empty(element)) => {
                for attribute in element.attributes() {
                    let attribute = attribute.ok()?;
                    let decoded = reader.decoder().decode(attribute.value.as_ref()).ok()?;
                    let decoded = unescape(&decoded).ok()?.into_owned();
                    if attribute.key.as_ref() == b"LyricContent" && lyric_content.is_none() {
                        lyric_content = Some(decoded);
                    }
                }
            }
            Ok(Event::Text(text)) => {
                let decoded = reader.decoder().decode(text.as_ref()).ok()?;
                unescape(&decoded).ok()?;
            }
            Ok(Event::Eof) => return lyric_content,
            Err(_) => return None,
            _ => {}
        }
    }
}

fn extract_qrc_content_relaxed(raw: &str) -> Option<String> {
    const TAG_NAME: &str = "<Lyric_1";
    let bytes = raw.as_bytes();
    let mut search_from = 0;

    while let Some(offset) = raw[search_from..].find(TAG_NAME) {
        let mut cursor = search_from + offset + TAG_NAME.len();
        if bytes
            .get(cursor)
            .is_some_and(|byte| is_xml_name_byte(*byte))
        {
            search_from = cursor;
            continue;
        }

        loop {
            skip_ascii_whitespace(bytes, &mut cursor);
            if cursor >= bytes.len() || bytes[cursor] == b'>' || bytes[cursor..].starts_with(b"/>")
            {
                break;
            }

            let name_start = cursor;
            while bytes
                .get(cursor)
                .is_some_and(|byte| is_xml_name_byte(*byte))
            {
                cursor += 1;
            }
            if cursor == name_start {
                break;
            }
            let name = &raw[name_start..cursor];

            skip_ascii_whitespace(bytes, &mut cursor);
            if bytes.get(cursor) != Some(&b'=') {
                break;
            }
            cursor += 1;
            skip_ascii_whitespace(bytes, &mut cursor);
            let Some(quote @ (b'"' | b'\'')) = bytes.get(cursor).copied() else {
                break;
            };
            cursor += 1;
            let value_start = cursor;

            if name == "LyricContent" {
                while let Some(relative_end) =
                    bytes[cursor..].iter().position(|byte| *byte == quote)
                {
                    let value_end = cursor + relative_end;
                    if qrc_attribute_tail_is_valid(&raw[value_end + 1..]) {
                        return Some(decode_qrc_entities(&raw[value_start..value_end]));
                    }
                    cursor = value_end + 1;
                }
                break;
            }

            let Some(relative_end) = bytes[cursor..].iter().position(|byte| *byte == quote) else {
                break;
            };
            cursor += relative_end + 1;
        }

        search_from = search_from + offset + TAG_NAME.len();
    }

    None
}

fn qrc_attribute_tail_is_valid(input: &str) -> bool {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    loop {
        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes[cursor..].starts_with(b"/>") || bytes.get(cursor) == Some(&b'>') {
            return true;
        }

        let name_start = cursor;
        while bytes
            .get(cursor)
            .is_some_and(|byte| is_xml_name_byte(*byte))
        {
            cursor += 1;
        }
        if cursor == name_start {
            return false;
        }

        skip_ascii_whitespace(bytes, &mut cursor);
        if bytes.get(cursor) != Some(&b'=') {
            return false;
        }
        cursor += 1;
        skip_ascii_whitespace(bytes, &mut cursor);
        let Some(quote @ (b'"' | b'\'')) = bytes.get(cursor).copied() else {
            return false;
        };
        cursor += 1;
        let Some(relative_end) = bytes[cursor..].iter().position(|byte| *byte == quote) else {
            return false;
        };
        cursor += relative_end + 1;
    }
}

fn skip_ascii_whitespace(bytes: &[u8], cursor: &mut usize) {
    while bytes
        .get(*cursor)
        .is_some_and(|byte| byte.is_ascii_whitespace())
    {
        *cursor += 1;
    }
}

fn is_xml_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'.' | b'-')
}

fn decode_qrc_entities(input: &str) -> String {
    let mut decoded = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(relative_start) = input[cursor..].find('&') {
        let entity_start = cursor + relative_start;
        decoded.push_str(&input[cursor..entity_start]);
        let Some(relative_end) = input[entity_start + 1..].find(';') else {
            decoded.push_str(&input[entity_start..]);
            return decoded;
        };
        let entity_end = entity_start + 1 + relative_end;
        let entity = &input[entity_start + 1..entity_end];
        if let Some(character) = decode_qrc_entity(entity) {
            decoded.push(character);
            cursor = entity_end + 1;
        } else {
            decoded.push('&');
            cursor = entity_start + 1;
        }
    }

    decoded.push_str(&input[cursor..]);
    decoded
}

fn decode_qrc_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "apos" => Some('\''),
        "gt" => Some('>'),
        "lt" => Some('<'),
        "quot" => Some('"'),
        _ => entity
            .strip_prefix("#x")
            .or_else(|| entity.strip_prefix("#X"))
            .and_then(|digits| u32::from_str_radix(digits, 16).ok())
            .or_else(|| {
                entity
                    .strip_prefix('#')
                    .and_then(|digits| digits.parse::<u32>().ok())
            })
            .and_then(char::from_u32),
    }
}

fn parse_qrc_line_timestamp(input: &str) -> Option<(Duration, usize)> {
    let end = input.strip_prefix('[')?.find(']')? + 1;
    let (start, duration) = input[1..end].split_once(',')?;
    let start = start.parse::<u64>().ok()?;
    duration.parse::<u64>().ok()?;
    Some((Duration::from_millis(start), end + 1))
}

fn parse_qrc_words(input: &str) -> (String, Vec<LyricWord>) {
    let mut text = String::with_capacity(input.len());
    let mut words = Vec::new();
    let mut cursor = 0;
    while let Some(open) = input[cursor..].find('(').map(|offset| cursor + offset) {
        let Some(close) = input[open + 1..].find(')').map(|offset| open + 1 + offset) else {
            break;
        };
        if let Some((start, duration)) = parse_qrc_word_timestamp(&input[open + 1..close]) {
            let range_start = text.len();
            text.push_str(&input[cursor..open]);
            let range_end = text.len();
            if range_start < range_end {
                words.push(LyricWord {
                    range: range_start..range_end,
                    start,
                    end: start.saturating_add(duration),
                    ruby: None,
                });
            }
            cursor = close + 1;
        } else {
            text.push_str(&input[cursor..=open]);
            cursor = open + 1;
        }
    }
    text.push_str(&input[cursor..]);
    trim_lyric_text(text, words)
}

fn parse_qrc_word_timestamp(input: &str) -> Option<(Duration, Duration)> {
    let mut parts = input.split(',');
    let start = parts.next()?.parse::<u64>().ok()?;
    let duration = parts.next()?.parse::<u64>().ok()?;
    Some((
        Duration::from_millis(start),
        Duration::from_millis(duration),
    ))
}

fn trim_lyric_text(text: String, words: Vec<LyricWord>) -> (String, Vec<LyricWord>) {
    let start = text.len() - text.trim_start().len();
    let end = text.trim_end().len();
    if start >= end {
        return (String::new(), Vec::new());
    }
    if start == 0 && end == text.len() {
        return (text, words);
    }

    let words = words
        .into_iter()
        .filter_map(|word| {
            let range_start = word.range.start.clamp(start, end);
            let range_end = word.range.end.clamp(start, end);
            (range_start < range_end).then_some(LyricWord {
                range: range_start - start..range_end - start,
                start: word.start,
                end: word.end,
                ruby: word.ruby,
            })
        })
        .collect();
    (text[start..end].to_owned(), words)
}

fn parse_lrc(raw: &str) -> Vec<LyricLine> {
    let mut lines = Vec::new();
    for raw_line in raw.lines() {
        let mut remainder = raw_line.trim_start_matches('\u{feff}');
        let mut timestamps = Vec::new();
        while let Some((timestamp, consumed)) = parse_lrc_timestamp(remainder) {
            timestamps.push(timestamp);
            remainder = &remainder[consumed..];
        }
        let text = remainder.trim();
        if text.is_empty() {
            continue;
        }
        lines.extend(timestamps.into_iter().map(|start| LyricLine {
            start,
            text: text.to_owned(),
            words: Vec::new(),
            translation: None,
        }));
    }
    lines.sort_by_key(|line| line.start);
    lines
}

fn parse_lrc_timestamp(input: &str) -> Option<(Duration, usize)> {
    if !input.starts_with('[') {
        return None;
    }
    let end = input.find(']')?;
    let (minutes, seconds) = input[1..end].split_once(':')?;
    let minutes = minutes.parse::<u64>().ok()?;
    let (seconds, fraction) = seconds.split_once('.').unwrap_or((seconds, ""));
    let seconds = seconds.parse::<u64>().ok()?;
    if seconds >= 60 || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let milliseconds = match fraction.len() {
        0 => 0,
        1 => fraction.parse::<u64>().ok()? * 100,
        2 => fraction.parse::<u64>().ok()? * 10,
        _ => fraction[..3].parse::<u64>().ok()?,
    };
    Some((
        Duration::from_millis(
            minutes
                .saturating_mul(60_000)
                .saturating_add(seconds.saturating_mul(1_000))
                .saturating_add(milliseconds),
        ),
        end + 1,
    ))
}

#[cfg(test)]
mod tests {
    use super::{extract_qrc_content, parse_lyrics};
    use std::time::Duration;

    #[test]
    fn parses_and_aligns_timestamped_lyrics() {
        let lyrics = parse_lyrics(
            "[ti:title]\n[00:01.20]first\n[00:10.000][00:20.000]repeat",
            Some("[00:01.200]第一句\n[00:10.000]重复"),
            None,
        );

        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].start, Duration::from_millis(1_200));
        assert_eq!(lyrics.lines[0].text, "first");
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("第一句"));
        assert_eq!(lyrics.lines[2].start, Duration::from_secs(20));
        assert_eq!(lyrics.active_index(Duration::from_secs(5)), Some(0));
        assert_eq!(lyrics.active_index(Duration::from_secs(10)), Some(1));
    }

    #[test]
    fn aligns_qrc_translation_with_rounded_timestamps() {
        let lyrics = parse_lyrics(
            r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[16013,500]原(16013,500)&#10;[22564,500]文(22564,500)"/></LyricInfo></QrcInfos>"#,
            Some("[00:16.010]第一行\n[00:22.560]第二行"),
            None,
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].translation.as_deref(), Some("第一行"));
        assert_eq!(lyrics.lines[1].translation.as_deref(), Some("第二行"));
    }

    #[test]
    fn aligns_qrc_romanization_as_kanji_ruby() {
        let lyrics = parse_lyrics(
            "[17499,5352]あ(17499,127)と(17626,211)一(17838,646)匙(18485,739)の(19224,234)憂(19459,297)鬱(19756,389)で(20146,285)",
            None,
            Some(
                "[17499,5352]a (17499,127)to (17626,211)hi (17838,323)to (18161,323)sa (18485,369)ji (18854,370)no (19224,234)yu (19459,148)u (19607,149)u (19756,194)tsu (19950,195)de (20146,285)",
            ),
        );

        let words = &lyrics.lines[0].words;
        assert_eq!(words[0].ruby, None);
        assert_eq!(words[2].ruby.as_deref(), Some("ひと"));
        assert_eq!(words[3].ruby.as_deref(), Some("さじ"));
        assert_eq!(words[5].ruby.as_deref(), Some("ゆう"));
        assert_eq!(words[6].ruby.as_deref(), Some("うつ"));
    }

    #[test]
    fn assigns_qrc_romanization_to_the_word_with_the_largest_overlap() {
        let lyrics = parse_lyrics(
            "[4244,4592]夜(4244,349)の(4593,406)赤(4999,373)と(5372,1039)歩(6411,433)き(6844,756)解(7600,448)く(8048,788)",
            None,
            Some(
                "[4243,4593]yo(4243,182)ru(4426,166)no(4592,406)a(4999,164)ka(5163,208)to(5371,1038)a(6410,236)ru(6646,197)ki(6843,756)ho(7600,236)do(7836,211)ku(8047,788)",
            ),
        );

        let words = &lyrics.lines[0].words;
        assert_eq!(words[0].ruby.as_deref(), Some("よる"));
        assert_eq!(words[1].ruby, None);
        assert_eq!(words[2].ruby.as_deref(), Some("あか"));
        assert_eq!(words[3].ruby, None);
        assert_eq!(words[4].ruby.as_deref(), Some("ある"));
        assert_eq!(words[5].ruby, None);
        assert_eq!(words[6].ruby.as_deref(), Some("ほど"));
        assert_eq!(words[7].ruby, None);
    }

    #[test]
    fn does_not_treat_chinese_romanization_as_japanese_ruby() {
        let lyrics = parse_lyrics(
            "[1000,500]中文(1000,500)",
            None,
            Some("[1000,500]zhong (1000,250)wen (1250,250)"),
        );

        assert_eq!(lyrics.lines[0].words[0].ruby, None);
    }

    #[test]
    fn preserves_literal_line_breaks_in_qrc_attributes() {
        let lyrics = parse_lyrics(
            "<QrcInfos><LyricInfo><Lyric_1 LyricContent=\"[ti:title]\r\n[0,500]中(0,250)文(250,250)\r\n[500,500]歌词(500,500)\"/></LyricInfo></QrcInfos>",
            None,
            None,
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].text, "中文");
        assert_eq!(lyrics.lines[1].text, "歌词");
    }

    #[test]
    fn extracts_qrc_content_with_malformed_xml_characters() {
        let content = extract_qrc_content(
            r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[0,500]Recording & Mix <live>(0,500)&#10;[500,500]Marcus "MarcLo" Lomax &amp; &#x41;&apos;s(500,500)" Source='qq'/></LyricInfo></QrcInfos>"#,
        )
        .unwrap();

        assert_eq!(
            content,
            "[0,500]Recording & Mix <live>(0,500)\n[500,500]Marcus \"MarcLo\" Lomax & A's(500,500)"
        );
    }

    #[test]
    fn parses_all_lines_after_an_unescaped_qrc_quote() {
        let lyrics = parse_lyrics(
            r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[0,500]Marcus "MarcLo" Lomax(0,500)&#10;[500,500]second(500,500)&#10;[1000,500]third(1000,500)"/></LyricInfo></QrcInfos>"#,
            None,
            None,
        );

        assert_eq!(lyrics.lines.len(), 3);
        assert_eq!(lyrics.lines[0].text, "Marcus \"MarcLo\" Lomax");
        assert_eq!(lyrics.lines[2].text, "third");
    }

    #[test]
    fn falls_back_to_lrc_inside_the_qrc_wrapper() {
        let lyrics = parse_lyrics(
            r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[00:01.000]first&#10;[00:02.000]second"/></LyricInfo></QrcInfos>"#,
            None,
            None,
        );

        assert_eq!(lyrics.lines.len(), 2);
        assert_eq!(lyrics.lines[0].text, "first");
        assert_eq!(lyrics.lines[1].text, "second");
    }

    #[test]
    fn rejects_a_truncated_qrc_wrapper_instead_of_returning_a_prefix() {
        assert_eq!(
            extract_qrc_content(
                r#"<QrcInfos><LyricInfo><Lyric_1 LyricContent="[0,500]first(0,500)"#
            ),
            None
        );
    }

    #[test]
    fn omits_provider_translation_placeholders() {
        let lyrics = parse_lyrics(
            "[00:01.000]词：作者\n[00:02.000]//",
            Some("[00:01.000]//\n[00:02.000]实际翻译"),
            None,
        );

        assert_eq!(lyrics.lines[0].translation, None);
        assert_eq!(lyrics.lines[1].text, "//");
        assert_eq!(lyrics.lines[1].translation.as_deref(), Some("实际翻译"));
    }
}
