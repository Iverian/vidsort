use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;

use crate::parser::movie::MovieMetadata;
use crate::parser::tvshow::EpisodeId;
use crate::parser::tvshow::EpisodeMetadata;
use crate::report::AnyResult;
use crate::tvdb;
use crate::tvdb::MovieMeta;
use crate::tvdb::ShowMeta;
use crate::types::TorrentFile;
use crate::types::TorrentInfo;

const VIDEO_EXTENSIONS: &[&str] = &["mkv", "mp4", "avi", "mov", "wmv", "m4v"];

const SAMPLE_SIZE_THRESHOLD: u64 = 50 * 1024 * 1024; // 50 MB

static EXTRAS_PAT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(featurettes?|trailer|behind[._-]the[._-]scenes|deleted[._-]scene|interview)\b",
    )
    .unwrap()
});

/// Result of classifying a torrent.
///
/// `all_files` always carries every file from the original torrent (relative
/// paths intact) so the pipeline can fall back to verbatim "other" linking
/// without losing NFOs, subtitles, or any non-video content.
#[derive(Debug)]
pub struct Classification {
    pub all_files: Vec<TorrentFile>,
    pub kind: ClassificationKind,
}

#[derive(Debug)]
pub enum ClassificationKind {
    Show(ShowResult),
    Movie(MovieResult),
    Other,
}

#[derive(Debug, Clone)]
pub struct ShowResult {
    pub meta: ShowMeta,
    /// Video files only (extras/samples excluded), each paired with its
    /// pre-parsed episode ID.  Avoids re-parsing filenames in the linker.
    pub video_files: Vec<ShowFile>,
}

/// A video file from a show torrent paired with its pre-parsed episode ID.
/// Falls back to the torrent-level season/episode when the filename does not
/// contain a parseable episode marker.
#[derive(Debug, Clone)]
pub struct ShowFile {
    pub file: TorrentFile,
    pub episode_id: EpisodeId,
}

#[derive(Debug, Clone)]
pub struct MovieResult {
    pub meta: MovieMeta,
    /// Video files only (extras/samples excluded) — used for linking.
    pub video_files: Vec<TorrentFile>,
}

/// Classify a torrent using file-name heuristics, then confirm and enrich via
/// TVDB.
///
/// Flow:
/// 1. Parse as TV show → query TVDB for series metadata → if found, return
///    `Show`.
/// 2. Parse as movie (skipped when episode markers are present) → query TVDB
///    for movie metadata → if found, return `Movie`.
/// 3. Return `Other`.
pub async fn classify(
    info: TorrentInfo,
    tvdb: &tvdb::Client,
    imdb_blacklist: &[String],
) -> AnyResult<Classification> {
    let TorrentInfo {
        files: all_files, ..
    } = info;
    let video_files = filter_video_files(&all_files);

    if video_files.is_empty() {
        return Ok(Classification {
            all_files,
            kind: ClassificationKind::Other,
        });
    }

    if let Some(candidate) = try_show(&video_files)
        && let Some(meta) = tvdb
            .enrich_show(
                &candidate.raw_title,
                None,
                candidate.season,
                candidate.episode,
            )
            .await?
    {
        if is_blacklisted(meta.imdb_id.as_deref(), imdb_blacklist) {
            tracing::info!(imdb_id = ?meta.imdb_id, "show IMDB ID is blacklisted; treating as Other");
        } else {
            return Ok(Classification {
                all_files,
                kind: ClassificationKind::Show(ShowResult {
                    meta,
                    video_files: candidate.video_files,
                }),
            });
        }
    }

    if !has_episode_markers(&video_files)
        && let Some(candidate) = try_movie(&video_files)
        && let Some(meta) = tvdb
            .enrich_movie(&candidate.raw_title, candidate.year_hint)
            .await?
    {
        if is_blacklisted(meta.imdb_id.as_deref(), imdb_blacklist) {
            tracing::info!(imdb_id = ?meta.imdb_id, "movie IMDB ID is blacklisted; treating as Other");
        } else {
            return Ok(Classification {
                all_files,
                kind: ClassificationKind::Movie(MovieResult {
                    meta,
                    video_files: candidate.video_files,
                }),
            });
        }
    }

    Ok(Classification {
        all_files,
        kind: ClassificationKind::Other,
    })
}

fn is_blacklisted(imdb_id: Option<&str>, blacklist: &[String]) -> bool {
    imdb_id.is_some_and(|id| blacklist.iter().any(|b| b == id))
}

// ── internal heuristic types ──────────────────────────────────────────────────

struct ShowCandidate {
    raw_title: String,
    season: u32,
    episode: u32,
    video_files: Vec<ShowFile>,
}

struct MovieCandidate {
    raw_title: String,
    year_hint: Option<u32>,
    video_files: Vec<TorrentFile>,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn filter_video_files(files: &[TorrentFile]) -> Vec<TorrentFile> {
    files
        .iter()
        .filter(|f| is_video(f) && !is_sample(f) && !is_extra(f))
        .cloned()
        .collect()
}

fn is_video(file: &TorrentFile) -> bool {
    file.name
        .extension()
        .is_some_and(|ext| VIDEO_EXTENSIONS.contains(&ext))
}

fn is_sample(file: &TorrentFile) -> bool {
    if file.length < SAMPLE_SIZE_THRESHOLD {
        return true;
    }
    file.name
        .file_stem()
        .is_some_and(|s| s.to_lowercase().contains("sample"))
}

fn is_extra(file: &TorrentFile) -> bool {
    if file
        .name
        .file_stem()
        .is_some_and(|s| EXTRAS_PAT.is_match(s))
    {
        return true;
    }
    // Also treat files nested under an extras directory (e.g. "Featurettes/").
    file.name
        .parent()
        .into_iter()
        .flat_map(|p| p.components())
        .any(|c| EXTRAS_PAT.is_match(c.as_str()))
}

fn try_show(video_files: &[TorrentFile]) -> Option<ShowCandidate> {
    let largest = largest_file(video_files);

    let stem = largest.name.file_stem().unwrap_or("");
    let parent_parts: Vec<&str> = largest
        .name
        .parent()
        .into_iter()
        .flat_map(|p| p.components().map(|c| c.as_str()))
        .collect();

    // Try sources in order: file stem, then parent dirs (innermost first)
    let mut sources = vec![stem];
    sources.extend(parent_parts.iter().rev().copied());

    for source in sources {
        if let Some(meta) = EpisodeMetadata::from_filename(source) {
            // Leading-number format ("09 - Title") yields no show name — use the
            // outermost parent directory.  Without a parent dir we cannot determine
            // the title, so return None rather than guessing.
            let raw_title = if meta.show.is_empty() {
                (*parent_parts.first()?).to_owned()
            } else {
                meta.show
            };
            let fallback = meta.episode;
            let show_files: Vec<ShowFile> = video_files
                .iter()
                .map(|f| {
                    let stem = f.name.file_stem().unwrap_or("");
                    let episode_id =
                        EpisodeMetadata::from_filename(stem).map_or(fallback, |m| m.episode);
                    ShowFile {
                        file: f.clone(),
                        episode_id,
                    }
                })
                .collect();
            // Reject if any two files share the same episode ID — that indicates
            // quality/language variants or a mis-parsed torrent, not a clean pack.
            let unique_ids: HashSet<_> = show_files.iter().map(|sf| sf.episode_id).collect();
            if unique_ids.len() != show_files.len() {
                return None;
            }
            return Some(ShowCandidate {
                raw_title,
                season: fallback.season,
                episode: fallback.episode,
                video_files: show_files,
            });
        }
    }

    None
}

fn try_movie(video_files: &[TorrentFile]) -> Option<MovieCandidate> {
    // Movies must be a single video file; multiple files indicate a season pack
    // or a multi-part release that should not be classified as a movie.
    if video_files.len() != 1 {
        return None;
    }
    let stem = video_files[0].name.file_stem().unwrap_or("");
    MovieMetadata::from_filename(stem).map(|meta| MovieCandidate {
        raw_title: meta.title,
        year_hint: meta.year,
        video_files: video_files.to_vec(),
    })
}

fn has_episode_markers(files: &[TorrentFile]) -> bool {
    let stem = largest_file(files).name.file_stem().unwrap_or("");
    EpisodeMetadata::from_filename(stem).is_some()
}

fn largest_file(files: &[TorrentFile]) -> &TorrentFile {
    files
        .iter()
        .max_by_key(|f| f.length)
        .expect("called with non-empty slice")
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::types::TorrentFile;

    fn mkfile(name: &str, mb: u64) -> TorrentFile {
        TorrentFile {
            name: Utf8PathBuf::from(name),
            length: mb * 1024 * 1024,
        }
    }

    // --- filtering ---

    #[test]
    fn no_video_files_gives_empty() {
        let files = vec![mkfile("readme.txt", 1)];
        assert!(filter_video_files(&files).is_empty());
    }

    #[test]
    fn all_samples_gives_empty() {
        let files = vec![mkfile("sample.mkv", 10), mkfile("extra.nfo", 1)];
        assert!(filter_video_files(&files).is_empty());
    }

    #[test]
    fn extras_only_gives_empty() {
        let files = vec![
            mkfile("featurette.mkv", 200),
            mkfile("trailer.mkv", 100),
            mkfile("interview.mkv", 80),
        ];
        assert!(filter_video_files(&files).is_empty());
    }

    #[test]
    fn sample_excluded_from_video_files() {
        let files = vec![
            mkfile("Harrow.S01E01.720p.mkv", 700),
            mkfile("sample.mkv", 10),
        ];
        assert_eq!(filter_video_files(&files).len(), 1);
    }

    #[test]
    fn featurettes_directory_excluded() {
        // Real-world season pack: episode files alongside a Featurettes/ subtree.
        // Files inside Featurettes/ must be filtered out even when their own
        // filename doesn't match any extras keyword.
        let root =
            "Nurse Jackie (2009) Season 1-7 S01-S07 (1080p BluRay x265 HEVC 10bit AAC 7.1 Panda)";
        let files = vec![
            mkfile(
                &format!(
                    "{root}/Season 7/Nurse Jackie (2009) - S07E12 - I Say a Little Prayer (1080p BluRay x265 Panda).mkv"
                ),
                2_000,
            ),
            mkfile(
                &format!("{root}/Featurettes/Season 1/Prepping Nurse Jackie.mkv"),
                300,
            ),
            mkfile(
                &format!("{root}/Featurettes/Season 1/Unsung Heroes.mkv"),
                300,
            ),
            mkfile(
                &format!("{root}/Featurettes/Season 2/All About Eve.mkv"),
                300,
            ),
            mkfile(
                &format!("{root}/Featurettes/Season 5/Deleted Scenes - Season 5.mkv"),
                300,
            ),
        ];
        let video = filter_video_files(&files);
        assert_eq!(video.len(), 1);
        assert!(video[0].name.as_str().contains("S07E12"));
    }

    // --- show ---

    #[test]
    fn single_episode_file() {
        let files = vec![mkfile("Harrow.S02E10.720p.WEBRip.x264-GalaxyTV.mkv", 700)];
        let c = try_show(&files).expect("expected show candidate");
        assert_eq!(c.raw_title, "Harrow");
        assert_eq!(c.season, 2);
        assert_eq!(c.episode, 10);
        assert_eq!(c.video_files.len(), 1);
        assert_eq!(c.video_files[0].episode_id.season, 2);
        assert_eq!(c.video_files[0].episode_id.episode, 10);
    }

    #[test]
    fn season_pack_show() {
        let files = vec![
            mkfile("Harrow/Harrow.S01E01.720p.WEBRip.mkv", 700),
            mkfile("Harrow/Harrow.S01E02.720p.WEBRip.mkv", 700),
            mkfile("Harrow/Harrow.S01E03.720p.WEBRip.mkv", 700),
        ];
        let c = try_show(&files).expect("expected show candidate");
        assert_eq!(c.raw_title, "Harrow");
        assert_eq!(c.season, 1);
        assert_eq!(c.video_files.len(), 3);
        assert!(
            c.video_files
                .iter()
                .enumerate()
                .all(|(i, sf)| sf.episode_id.episode == (i + 1) as u32)
        );
    }

    #[test]
    fn show_episode_title_from_parent_dir_when_file_has_leading_number() {
        let files = vec![mkfile(
            "Sullivan's Crossing/09 - Can't Help Falling.mkv",
            1_500,
        )];
        let c = try_show(&files).expect("expected show candidate");
        assert_eq!(c.raw_title, "Sullivan's Crossing");
        assert_eq!(c.season, 1);
        assert_eq!(c.episode, 9);
    }

    #[test]
    fn flat_file_with_leading_number_and_no_parent_is_none() {
        let files = vec![mkfile("09 - Can't Help Falling.mkv", 1_500)];
        assert!(try_show(&files).is_none());
    }

    #[test]
    fn show_title_from_parent_directory() {
        let files = vec![mkfile(
            "Being Human US/Being.Human.US.S04E11.Ramona.the.Pest.1080p.mkv",
            1_500,
        )];
        let c = try_show(&files).expect("expected show candidate");
        assert_eq!(c.raw_title, "Being Human US");
        assert_eq!((c.season, c.episode), (4, 11));
    }

    #[test]
    fn year_in_show_title_preserved() {
        let files = vec![mkfile("Scrubs.2026.S01E05.1080p.x265-ELiTE.mkv", 800)];
        let c = try_show(&files).expect("expected show candidate");
        assert_eq!(c.raw_title, "Scrubs 2026");
        assert_eq!((c.season, c.episode), (1, 5));
    }

    #[test]
    fn show_duplicate_episode_ids_returns_none() {
        // Two files that both parse to S01E01 (e.g. different language tracks)
        // must not be accepted as a valid show candidate.
        let files = vec![
            mkfile("Show/Show.S01E01.EN.mkv", 700),
            mkfile("Show/Show.S01E01.RU.mkv", 700),
        ];
        assert!(try_show(&files).is_none());
    }

    // --- movie ---

    #[test]
    fn single_movie_file() {
        let files = vec![mkfile("Inception.2010.1080p.BluRay.x264.mkv", 8_000)];
        let c = try_movie(&files).expect("expected movie candidate");
        assert_eq!(c.raw_title, "Inception");
        assert_eq!(c.year_hint, Some(2010));
        assert_eq!(c.video_files.len(), 1);
    }

    #[test]
    fn movie_extras_excluded_from_video_files() {
        let all = vec![
            mkfile("Blade.1998.1080p.BluRay.mkv", 8_000),
            mkfile("featurette.mkv", 500),
            mkfile("trailer.mkv", 200),
        ];
        let video = filter_video_files(&all);
        let c = try_movie(&video).expect("expected movie candidate");
        assert_eq!(c.raw_title, "Blade");
        assert_eq!(c.video_files.len(), 1);
        assert_eq!(all.len(), 3); // all_files preserved by caller
    }

    #[test]
    fn movie_no_year() {
        let files = vec![mkfile("Oppenheimer.HEVC.x265.mkv", 12_000)];
        let c = try_movie(&files).expect("expected movie candidate");
        assert_eq!(c.raw_title, "Oppenheimer");
        assert_eq!(c.year_hint, None);
    }

    #[test]
    fn movie_multi_file_returns_none() {
        // Two video files → not a movie (season pack or multi-part release)
        let files = vec![
            mkfile("Inception.2010.Part1.1080p.BluRay.mkv", 8_000),
            mkfile("Inception.2010.Part2.1080p.BluRay.mkv", 8_000),
        ];
        assert!(try_movie(&files).is_none());
    }
}
