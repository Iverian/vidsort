use std::sync::LazyLock;

use regex::Regex;

const TITLE_SEPARATORS: [char; 4] = [' ', '.', '_', '-'];

/// `SxxExx` and all common variants — space, dot, or dash between the S-part and E-part:
///   S01E05  S04 E04  S02.E05  S02-E05
static FULL_ID_PAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[sS](\d+)[\s._-]*[eE](\d+)").unwrap());

/// Standalone Exx with no preceding season number (season 1 implied).
/// `\b` word boundary prevents matching mid-word (e.g. "EDGE2020" has E followed by
/// non-digits; `WEBRip` has no E followed by digits at all).
static EP_ONLY_PAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b[eE](\d+)\b").unwrap());

/// Leading "NN - " episode number used by some releases (e.g. "09 - Title").
/// Season 1 implied; show title must come from a different source (torrent name / directory).
static LEADING_NUM_PAT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{1,3})\s*[-–]\s*\S").unwrap());

/// Year in parentheses used as a show disambiguator (e.g. "ER (1994)").
/// Stripped from the title prefix before normalization — it is metadata, not part of the name.
static YEAR_PARENS_PAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(\d{4}\)").unwrap());

#[derive(Debug, Clone)]
pub struct EpisodeMetadata {
    pub show: String,
    pub episode: EpisodeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EpisodeId {
    pub season: u32,
    pub episode: u32,
}

impl EpisodeMetadata {
    /// Parse episode metadata from a filename stem (no extension).
    ///
    /// Title is extracted from the portion of the stem that precedes the episode
    /// marker. For the leading-number format the title will be empty — callers
    /// should fall back to the torrent name or parent directory for the show name.
    pub fn from_filename(value: &str) -> Option<Self> {
        if let Some(cap) = FULL_ID_PAT.captures(value) {
            let season: u32 = cap[1].parse().ok()?;
            let episode: u32 = cap[2].parse().ok()?;
            let show = normalize_title(&value[..cap.get(0)?.start()]);
            return Some(Self {
                show,
                episode: EpisodeId { season, episode },
            });
        }

        if let Some(cap) = EP_ONLY_PAT.captures(value) {
            let episode: u32 = cap[1].parse().ok()?;
            let show = normalize_title(&value[..cap.get(0)?.start()]);
            return Some(Self {
                show,
                episode: EpisodeId { season: 1, episode },
            });
        }

        if let Some(cap) = LEADING_NUM_PAT.captures(value) {
            let episode: u32 = cap[1].parse().ok()?;
            return Some(Self {
                show: String::new(),
                episode: EpisodeId { season: 1, episode },
            });
        }

        None
    }
}

impl EpisodeId {
    pub fn from_filename(value: &str) -> Option<Self> {
        EpisodeMetadata::from_filename(value).map(|m| m.episode)
    }
}

fn normalize_title(raw: &str) -> String {
    let raw = YEAR_PARENS_PAT.replace_all(raw, "");
    raw.split(TITLE_SEPARATORS)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::EpisodeMetadata;

    fn parse(input: &str) -> Option<(String, u32, u32)> {
        EpisodeMetadata::from_filename(input).map(|m| (m.show, m.episode.season, m.episode.episode))
    }

    // --- format variants ---

    #[test]
    fn standard_sxxexx_dots() {
        let (show, s, e) = parse("Harrow.S02E10.720p.WEBRip.x264-GalaxyTV").unwrap();
        assert_eq!(show, "Harrow");
        assert_eq!((s, e), (2, 10));
    }

    #[test]
    fn sxx_space_exx() {
        let (show, s, e) = parse("SOUTHLAND - S04 E04 - Identity (720p Web-DL)").unwrap();
        assert_eq!(show, "SOUTHLAND");
        assert_eq!((s, e), (4, 4));
    }

    #[test]
    fn sxx_dot_exx() {
        let (show, s, e) = parse("Vampiry.srednej.polosy.S02.E05.2022.WEB-DL.1080p").unwrap();
        assert_eq!(show, "Vampiry srednej polosy");
        assert_eq!((s, e), (2, 5));
    }

    #[test]
    fn episode_only_no_season() {
        let (show, s, e) = parse("Vampiry.srednej.polosy.E08.2020.WEB-DL.1080p.ExKinoRay").unwrap();
        assert_eq!(show, "Vampiry srednej polosy");
        assert_eq!((s, e), (1, 8));
    }

    #[test]
    fn leading_number_format() {
        let (show, s, e) = parse("09 - Can't Help Falling").unwrap();
        assert_eq!(show, ""); // title comes from directory/torrent name
        assert_eq!((s, e), (1, 9));
    }

    #[test]
    fn year_in_show_title() {
        // Year is part of the title, not a quality tag — must be preserved
        let (show, s, e) = parse("Scrubs.2026.S01E05.1080p.x265-ELiTE").unwrap();
        assert_eq!(show, "Scrubs 2026");
        assert_eq!((s, e), (1, 5));
    }

    #[test]
    fn year_in_parens_before_episode() {
        let (show, s, e) =
            parse("ER (1994) - S01E10 - Blizzard (1080p AMZN WEB-DL x265 MONOLITH)").unwrap();
        assert_eq!(show, "ER");
        assert_eq!((s, e), (1, 10));
    }

    #[test]
    fn dash_separator_in_title() {
        // Dash used as word separator between title segments
        let (show, s, e) =
            parse("Star.Wars-The.Clone.Wars.S01E16-The.Hidden.Enemy.1080p.BRRip").unwrap();
        assert_eq!(show, "Star Wars The Clone Wars");
        assert_eq!((s, e), (1, 16));
    }

    #[test]
    fn multi_word_title_with_dots() {
        let (show, s, e) = parse(
            "Being.Human.US.S04E11.Ramona.the.Pest.1080p.AMZN.WEBRip.DDP.5.1.H.265.-EDGE2020",
        )
        .unwrap();
        assert_eq!(show, "Being Human US");
        assert_eq!((s, e), (4, 11));
    }

    #[test]
    fn no_episode_marker_returns_none() {
        assert!(parse("some.random.file.1080p.BluRay").is_none());
    }

    // --- real-world examples from data/shows.txt ---

    #[test]
    fn real_world_examples() {
        let examples: &[(&str, &str, u32, u32)] = &[
            // (stem, expected_show, season, episode)
            (
                "Harrow.S01E01.720p.AMZN.WEBRip.x264-GalaxyTV",
                "Harrow",
                1,
                1,
            ),
            (
                "Being.Human.US.S03E13.Ruh.Roh.1080p.AMZN.WEBRip.DDP.5.1.H.265.-EDGE2020",
                "Being Human US",
                3,
                13,
            ),
            (
                "SOUTHLAND - S04 E07 - Fallout (720p Web-DL)",
                "SOUTHLAND",
                4,
                7,
            ),
            (
                "Vampiry.srednej.polosy.S02.E03.2022.WEB-DL.1080p",
                "Vampiry srednej polosy",
                2,
                3,
            ),
            (
                "Vampiry.srednej.polosy.E06.2020.WEB-DL.1080p.ExKinoRay",
                "Vampiry srednej polosy",
                1,
                6,
            ),
            ("01 - Coming Home", "", 1, 1),
            ("Scrubs.2026.S01E01.1080p.x265-ELiTE", "Scrubs 2026", 1, 1),
            (
                "ER (1994) - S06E12 - Abby Road (1080p AMZN WEB-DL x265 MONOLITH)",
                "ER",
                6,
                12,
            ),
            (
                "Star.Wars-The.Clone.Wars.S01E22-Hostage.Crisis.1080p.BRRip.Opus51.x265.10bit-CAIRN",
                "Star Wars The Clone Wars",
                1,
                22,
            ),
            (
                "The.Mighty.Nein.S01E08.1080p.WEBRip.x265-KONTRAST",
                "The Mighty Nein",
                1,
                8,
            ),
        ];

        for &(stem, expected_show, expected_season, expected_ep) in examples {
            let result = parse(stem);
            match result {
                Some((show, s, e)) => println!(
                    "{stem:?}\n  => show={show:?}, S{s:02}E{e:02}  (expected {expected_show:?} S{expected_season:02}E{expected_ep:02})\n"
                ),
                None => println!("{stem:?}\n  => NONE\n"),
            }
            let (show, s, e) = parse(stem).unwrap_or_else(|| panic!("no match for {stem:?}"));
            assert_eq!(show, expected_show, "show mismatch for {stem:?}");
            assert_eq!(s, expected_season, "season mismatch for {stem:?}");
            assert_eq!(e, expected_ep, "episode mismatch for {stem:?}");
        }
    }
}
