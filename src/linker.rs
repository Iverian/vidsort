use std::fmt::Write as _;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use nix::errno::Errno;
use nix::fcntl::AT_FDCWD;
use nix::fcntl::AtFlags;

use crate::classifier::ShowFile;
use crate::config::DirConfig;
use crate::tvdb::MovieMeta;
use crate::tvdb::ShowMeta;
use crate::types::TorrentFile;

/// Destination path for a show file, relative to the shows base directory.
pub fn show_dest(meta: &ShowMeta, show_file: &ShowFile) -> Utf8PathBuf {
    let folder = folder_name(
        &meta.canonical_title,
        meta.release_year,
        meta.imdb_id.as_deref(),
    );
    let ext = show_file.file.name.extension().unwrap_or("");
    let season_dir = format!("Season {:02}", show_file.episode_id.season);
    let filename = format!(
        "{} S{:02}E{:02}.{}",
        meta.canonical_title, show_file.episode_id.season, show_file.episode_id.episode, ext
    );
    Utf8PathBuf::from(folder).join(season_dir).join(filename)
}

/// Destination path for a movie file, relative to the movies base directory.
pub fn movie_dest(meta: &MovieMeta, file: &TorrentFile) -> Utf8PathBuf {
    let folder = folder_name(
        &meta.canonical_title,
        meta.release_year,
        meta.imdb_id.as_deref(),
    );
    let ext = file.name.extension().unwrap_or("");
    let filename = format!("{}.{}", meta.canonical_title, ext);
    Utf8PathBuf::from(folder).join(filename)
}

#[tracing::instrument(skip_all, fields(title = %meta.canonical_title, season = meta.season, episode = meta.episode))]
pub fn link_show(meta: &ShowMeta, files: &[ShowFile], download_dir: &Utf8Path, dirs: &DirConfig) {
    for show_file in files {
        let src = download_dir.join(&show_file.file.name);
        let dst = dirs.shows.join(show_dest(meta, show_file));
        do_link(&src, &dst);
    }
}

#[tracing::instrument(skip_all, fields(title = %meta.canonical_title))]
pub fn link_movie(
    meta: &MovieMeta,
    files: &[TorrentFile],
    download_dir: &Utf8Path,
    dirs: &DirConfig,
) {
    for file in files {
        let src = download_dir.join(&file.name);
        let dst = dirs.movies.join(movie_dest(meta, file));
        do_link(&src, &dst);
    }
}

#[tracing::instrument(skip_all)]
pub fn link_other(files: &[TorrentFile], download_dir: &Utf8Path, dirs: &DirConfig) {
    for file in files {
        let src = download_dir.join(&file.name);
        // Preserve the torrent's relative path so multi-level directory
        // structures (season packs, extras folders, etc.) are replicated intact.
        let dst = dirs.other.join(&file.name);
        do_link(&src, &dst);
    }
}

fn folder_name(title: &str, year: Option<u32>, imdb_id: Option<&str>) -> String {
    let mut s = title.to_string();
    if let Some(y) = year {
        let tag = format!("({y})");
        if !s.ends_with(&tag) {
            write!(s, " ({y})").unwrap();
        }
    }
    if let Some(imdb) = imdb_id {
        write!(s, " [{imdb}]").unwrap();
    }
    s
}

#[tracing::instrument(fields(src = %src, dst = %dst))]
fn do_link(src: &Utf8Path, dst: &Utf8Path) {
    if let Some(parent) = dst.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::error!(path = %parent, error = ?e, "failed to create destination directory");
        return;
    }

    match nix::unistd::linkat(
        AT_FDCWD,
        src.as_std_path(),
        AT_FDCWD,
        dst.as_std_path(),
        AtFlags::empty(),
    ) {
        Ok(()) => {
            metrics::counter!("vidsort_links_created_total").increment(1);
            tracing::debug!("linked file");
        }
        Err(Errno::EEXIST) => {
            tracing::warn!("destination already exists, skipping");
        }
        Err(e) => {
            metrics::counter!("vidsort_link_errors_total").increment(1);
            tracing::error!(error = ?e, "linkat failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use camino::Utf8PathBuf;

    use super::*;
    use crate::classifier::ShowFile;
    use crate::config::DirConfig;
    use crate::parser::tvshow::EpisodeId;
    use crate::tvdb::MovieMeta;
    use crate::tvdb::ShowMeta;
    use crate::types::TorrentFile;

    fn dirs(tmp: &tempfile::TempDir) -> DirConfig {
        let base = camino::Utf8Path::from_path(tmp.path()).unwrap();
        DirConfig {
            movies: base.join("movies"),
            shows: base.join("shows"),
            other: base.join("other"),
        }
    }

    fn make_file(dir: &Utf8Path, rel: &str) -> TorrentFile {
        let abs = dir.join(rel);
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::File::create(&abs).unwrap();
        TorrentFile {
            name: Utf8PathBuf::from(rel),
            length: 1024,
        }
    }

    // ── show ────────────────────────────────────────────────────────────────

    #[test]
    fn link_show_creates_correct_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = camino::Utf8Path::from_path(tmp.path()).unwrap().join("dl");
        std::fs::create_dir_all(&dl).unwrap();

        let file = make_file(&dl, "Harrow.S02E10.720p.WEBRip.mkv");
        let show_file = ShowFile {
            file,
            episode_id: EpisodeId {
                season: 2,
                episode: 10,
            },
        };

        let meta = ShowMeta {
            canonical_title: "Harrow".to_string(),
            release_year: Some(2018),
            imdb_id: Some("tt6164502".to_string()),
            season: 2,
            episode: 10,
        };
        let d = dirs(&tmp);
        link_show(&meta, &[show_file], &dl, &d);

        let expected = d
            .shows
            .join("Harrow (2018) [tt6164502]")
            .join("Season 02")
            .join("Harrow S02E10.mkv");
        assert!(expected.exists(), "expected {expected} to exist");
    }

    #[test]
    fn link_show_season_pack_per_file_episode() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = camino::Utf8Path::from_path(tmp.path()).unwrap().join("dl");
        std::fs::create_dir_all(&dl).unwrap();

        let sf1 = ShowFile {
            file: make_file(&dl, "Harrow.S01E01.720p.mkv"),
            episode_id: EpisodeId {
                season: 1,
                episode: 1,
            },
        };
        let sf2 = ShowFile {
            file: make_file(&dl, "Harrow.S01E02.720p.mkv"),
            episode_id: EpisodeId {
                season: 1,
                episode: 2,
            },
        };

        let meta = ShowMeta {
            canonical_title: "Harrow".to_string(),
            release_year: Some(2018),
            imdb_id: None,
            season: 1,
            episode: 1,
        };
        let d = dirs(&tmp);
        link_show(&meta, &[sf1, sf2], &dl, &d);

        assert!(
            d.shows
                .join("Harrow (2018)")
                .join("Season 01")
                .join("Harrow S01E01.mkv")
                .exists()
        );
        assert!(
            d.shows
                .join("Harrow (2018)")
                .join("Season 01")
                .join("Harrow S01E02.mkv")
                .exists()
        );
    }

    #[test]
    fn link_show_exist_is_non_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = camino::Utf8Path::from_path(tmp.path()).unwrap().join("dl");
        std::fs::create_dir_all(&dl).unwrap();

        let show_file = ShowFile {
            file: make_file(&dl, "Show.S01E01.mkv"),
            episode_id: EpisodeId {
                season: 1,
                episode: 1,
            },
        };
        let meta = ShowMeta {
            canonical_title: "Show".to_string(),
            release_year: None,
            imdb_id: None,
            season: 1,
            episode: 1,
        };
        let d = dirs(&tmp);
        // Link twice — second call should not panic
        link_show(&meta, std::slice::from_ref(&show_file), &dl, &d);
        link_show(&meta, &[show_file], &dl, &d);
    }

    // ── movie ───────────────────────────────────────────────────────────────

    #[test]
    fn link_movie_creates_correct_path() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = camino::Utf8Path::from_path(tmp.path()).unwrap().join("dl");
        std::fs::create_dir_all(&dl).unwrap();

        let file = make_file(&dl, "Inception.2010.1080p.BluRay.mkv");

        let meta = MovieMeta {
            canonical_title: "Inception".to_string(),
            release_year: Some(2010),
            imdb_id: Some("tt1375666".to_string()),
        };
        let d = dirs(&tmp);
        link_movie(&meta, &[file], &dl, &d);

        let expected = d
            .movies
            .join("Inception (2010) [tt1375666]")
            .join("Inception.mkv");
        assert!(expected.exists(), "expected {expected} to exist");
    }

    #[test]
    fn link_movie_no_year_no_imdb() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = camino::Utf8Path::from_path(tmp.path()).unwrap().join("dl");
        std::fs::create_dir_all(&dl).unwrap();

        let file = make_file(&dl, "Alien.mkv");
        let meta = MovieMeta {
            canonical_title: "Alien".to_string(),
            release_year: None,
            imdb_id: None,
        };
        let d = dirs(&tmp);
        link_movie(&meta, &[file], &dl, &d);

        assert!(d.movies.join("Alien").join("Alien.mkv").exists());
    }

    // ── other ───────────────────────────────────────────────────────────────

    #[test]
    fn link_other_flat_file() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = camino::Utf8Path::from_path(tmp.path()).unwrap().join("dl");
        std::fs::create_dir_all(&dl).unwrap();

        let file = make_file(&dl, "some.random.file.mkv");
        let d = dirs(&tmp);
        link_other(&[file], &dl, &d);

        assert!(d.other.join("some.random.file.mkv").exists());
    }

    #[test]
    fn link_other_preserves_directory_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let dl = camino::Utf8Path::from_path(tmp.path()).unwrap().join("dl");
        std::fs::create_dir_all(&dl).unwrap();

        // Multi-level structure: torrent root dir / season dir / file
        let f1 = make_file(&dl, "Show Name/Season 01/Show.S01E01.mkv");
        let f2 = make_file(&dl, "Show Name/Season 01/Show.S01E02.mkv");
        let f3 = make_file(&dl, "Show Name/show.nfo");
        let d = dirs(&tmp);
        link_other(&[f1, f2, f3], &dl, &d);

        assert!(d.other.join("Show Name/Season 01/Show.S01E01.mkv").exists());
        assert!(d.other.join("Show Name/Season 01/Show.S01E02.mkv").exists());
        assert!(d.other.join("Show Name/show.nfo").exists());
    }

    // ── folder_name ──────────────────────────────────────────────────────────

    #[test]
    fn folder_name_all_fields() {
        assert_eq!(
            folder_name("Inception", Some(2010), Some("tt1375666")),
            "Inception (2010) [tt1375666]"
        );
    }

    #[test]
    fn folder_name_year_only() {
        assert_eq!(folder_name("Alien", Some(1979), None), "Alien (1979)");
    }

    #[test]
    fn folder_name_title_only() {
        assert_eq!(folder_name("Unknown", None, None), "Unknown");
    }

    #[test]
    fn folder_name_year_already_in_title() {
        // TVDB sometimes returns the year as part of the canonical title (e.g. "Paradise (2025)").
        // The year suffix must not be duplicated.
        assert_eq!(
            folder_name("Paradise (2025)", Some(2025), Some("tt27444205")),
            "Paradise (2025) [tt27444205]"
        );
    }
}
