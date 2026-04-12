use std::sync::LazyLock;

use regex::Regex;

const IN_SEPARATORS: [char; 2] = [' ', '.'];

static YEAR_PAT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b((?:19|20)\d{2})\b").unwrap());

/// Tokens that mark the start of quality/release metadata — everything from
/// the first match onward is discarded when extracting the raw title.
static QUALITY_TAG_PAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        \b(
            # resolution / scan
            [0-9]{3,4}[ip] | uhd | 4k | 2160p | 1440p
            # video codec
            | [xh]\.?26[45]
            | hevc | avc | xvid | divx
            # source
            | blu[-.]?ray | bluray | bdrip | bdrip | remux | dvdrip | dvdscr
            | web[-.]?dl | webrip | web | hdrip | hdtv | pdtv | ts | cam
            # audio
            | dts | dolby | truehd | atmos | aac | ac3 | dd[0-9p]* | flac
            | [257]\.[01]
            # hdr
            | hdr(?:10)? | dv | dolby[-.]?vision
            # release flags
            | repack | proper | extended | theatrical | dc | unrated
            | remastered | complete | season | S\d{2}
        )\b
        ",
    )
    .unwrap()
});

#[derive(Debug, Clone)]
pub struct MovieMetadata {
    pub title: String,
    pub year: Option<u32>,
}

impl MovieMetadata {
    pub fn from_filename(value: &str) -> Option<Self> {
        // Normalise dots/underscores to spaces
        let normalised = value.replace(['.', '_'], " ");

        // Find the first quality tag; everything before it is the title region
        let title_region = match QUALITY_TAG_PAT.find(&normalised) {
            Some(m) => &normalised[..m.start()],
            None => &normalised,
        };

        // Extract the most recent 4-digit year from the title region
        let year = YEAR_PAT
            .captures_iter(title_region)
            .last()
            .and_then(|c| c.get(1))
            .and_then(|m| m.as_str().parse::<u32>().ok());

        // Strip the year from the title, then normalise separators
        let title_without_year = match year {
            Some(y) => title_region.replace(&y.to_string(), ""),
            None => title_region.to_owned(),
        };

        let title: String = title_without_year
            .split(IN_SEPARATORS)
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        if title.is_empty() {
            return None;
        }

        Some(MovieMetadata { title, year })
    }
}

#[cfg(test)]
mod tests {
    use super::MovieMetadata;

    fn parse(input: &str) -> Option<(String, Option<u32>)> {
        MovieMetadata::from_filename(input).map(|m| (m.title, m.year))
    }

    #[test]
    fn simple_title_with_year() {
        let (title, year) = parse("Inception.2010.1080p.BluRay.x264").unwrap();
        assert_eq!(title, "Inception");
        assert_eq!(year, Some(2010));
    }

    #[test]
    fn title_with_spaces() {
        let (title, year) = parse("The Dark Knight 2008 720p BluRay").unwrap();
        assert_eq!(title, "The Dark Knight");
        assert_eq!(year, Some(2008));
    }

    #[test]
    fn title_no_year() {
        let (title, year) = parse("Oppenheimer.HEVC.x265.mkv").unwrap();
        assert_eq!(title, "Oppenheimer");
        assert_eq!(year, None);
    }

    #[test]
    fn title_with_year_in_parens_style() {
        // Year immediately followed by quality tag
        let (title, year) = parse("Interstellar.2014.BluRay.1080p.DTS").unwrap();
        assert_eq!(title, "Interstellar");
        assert_eq!(year, Some(2014));
    }

    #[test]
    fn web_dl_source() {
        let (title, year) = parse("Dune.Part.Two.2024.WEB-DL.1080p.H264").unwrap();
        assert_eq!(title, "Dune Part Two");
        assert_eq!(year, Some(2024));
    }

    #[test]
    fn multi_word_title_dots() {
        let (title, year) = parse("The.Grand.Budapest.Hotel.2014.1080p").unwrap();
        assert_eq!(title, "The Grand Budapest Hotel");
        assert_eq!(year, Some(2014));
    }

    #[test]
    fn empty_title_returns_none() {
        assert!(parse("1080p.BluRay.x264").is_none());
    }

    #[test]
    fn title_only_no_quality_tags() {
        let (title, year) = parse("Alien").unwrap();
        assert_eq!(title, "Alien");
        assert_eq!(year, None);
    }

    #[test]
    fn hdr_remux() {
        let (title, year) =
            parse("Everything.Everywhere.All.at.Once.2022.2160p.BluRay.REMUX.HDR").unwrap();
        assert_eq!(title, "Everything Everywhere All at Once");
        assert_eq!(year, Some(2022));
    }

    #[test]
    fn real_world_examples() {
        let examples = [
            "Alien 1979 DC REMASTERED 1080p BluRay HEVC x265 5.1 BONE.mkv",
            "Alien Romulus 2024 1080p BluRay HEVC x265 5.1 BONE.mkv",
            "Blade 1998 1080p BluRay HEVC x265 5.1 BONE.mkv",
            "Blade II 2002 1080p BluRay HEVC x265 5.1 BONE.mkv",
            "Blade Trinity 2004 UNRATED 1080p BluRay HEVC x265 5.1 BONE.mkv",
            "Code.3.2025.1080p.WEBRip.10Bit.DDP5.1.x265-NeoNoir.mkv",
            "Dracula A Love Tale 2025 1080p HDRip HC HEVC x265 5.1 BONE.mkv",
            "Enders Game 2013 UHD BluRay 1080p DD 5 1 DoVi HDR x265-SM737.mkv",
            "Interview.with.the.Vampire.1994.1080p.BluRay.x264-OFT.mkv",
            "Men in Black International 2019 1080p WEB-DL DD5 1 H264-FGT.mkv",
            "Predator Badlands 2025 1080p HDRip HEVC x265 BONE.mkv",
            "Rogue One A Star Wars Story 2016 1080p BluRay H264 AAC 5.1.eng.srt",
            "Rogue One A Star Wars Story 2016 1080p BluRay H264 AAC 5.1.mp4",
            "Sinners 2025 1080p BluRay x264 DD 5.1.mp4",
            "Star.Wars.Episode.II.Attack.Of.The.Clones.2002.REMASTERED.1080p.BluRay.H265.5.1-RBG.mp4",
            "Star.Wars.Episode.III.Revenge.Of.The.Sith.2005.REMASTERED.1080p.BluRay.H265.5.1-RBG.mp4",
            "Star.Wars.Episode.I.The.Phantom.Menace.1999.REMASTERED.1080p.BluRay.H265.5.1-RBG.eng.srt",
            "Star.Wars.Episode.I.The.Phantom.Menace.1999.REMASTERED.1080p.BluRay.H265.5.1-RBG.mp4",
            "Star.Wars.Episode.IV.A.New.Hope.1977.REMASTERED.1080p.BluRay.H265 5.1-RBG.mp4",
            "Star.Wars.Episode.VI.Return.Of.The.Jedi.1983.REMASTERED.1080p.BluRay.H265 5.1-RBG.mp4",
            "Star.Wars.Episode.V.The.Empire.Strikes.Back.1980.REMASTERED.1080p.BluRay.H265 5.1-RBG.mp4",
            "The.Lost.Boys.1987.1080p.BluRay.x264.YIFY.mp4",
            "The Shining 1980 REMASTERED 1080p BluRay HEVC x265 5.1 BONE.mkv",
            "The.Sixth.Sense.1999.1080p.BluRay.DDP5.1.x265.10bit-GalaxyRG265.mkv",
        ];

        for input in examples {
            // Strip extension before parsing (as classifier will do)
            let stem = input.rsplit_once('.').map_or(input, |(s, _)| s);
            match MovieMetadata::from_filename(stem) {
                Some(m) => println!("{input:?}\n  => title={:?}, year={:?}\n", m.title, m.year),
                None => println!("{input:?}\n  => NONE\n"),
            }
        }
    }
}
