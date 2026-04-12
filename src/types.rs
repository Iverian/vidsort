use camino::Utf8PathBuf;

#[derive(Debug, Clone, Copy)]
pub struct TorrentId(pub i64);

#[derive(Debug, Clone)]
pub struct TorrentFile {
    pub name: Utf8PathBuf,
    pub length: u64,
}

#[derive(Debug)]
pub struct TorrentInfo {
    pub id: TorrentId,
    /// Torrent display name from Transmission — used only for tracing/logging.
    pub name: String,
    pub download_dir: Utf8PathBuf,
    pub files: Vec<TorrentFile>,
}
