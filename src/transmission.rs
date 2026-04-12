use camino::Utf8PathBuf;
use transmission_rpc::TransClient;
use transmission_rpc::types::BasicAuth;
use transmission_rpc::types::Id;
use transmission_rpc::types::TorrentGetField;

use crate::config::TransmissionConfig;
use crate::report::AnyResult;
use crate::types::TorrentFile;
use crate::types::TorrentId;
use crate::types::TorrentInfo;

pub struct Client {
    inner: TransClient,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client").finish_non_exhaustive()
    }
}

impl Client {
    pub fn new(config: &TransmissionConfig) -> Self {
        let inner = match (&config.username, &config.password) {
            (Some(user), Some(password)) => TransClient::with_auth(
                config.url.clone(),
                BasicAuth {
                    user: user.clone(),
                    password: password.clone(),
                },
            ),
            _ => TransClient::new(config.url.clone()),
        };
        Self { inner }
    }

    /// Fetch all torrents that have finished downloading (`percentDone == 1.0`).
    #[tracing::instrument(skip(self))]
    pub async fn fetch_all_completed(&mut self) -> AnyResult<Vec<TorrentInfo>> {
        let fields = vec![
            TorrentGetField::Id,
            TorrentGetField::Name,
            TorrentGetField::DownloadDir,
            TorrentGetField::Files,
            TorrentGetField::PercentDone,
        ];

        let resp = self
            .inner
            .torrent_get(Some(fields), None)
            .await
            .map_err(|e| eyre::eyre!("transmission RPC request failed: {e}"))?;

        let mut result = Vec::new();
        for torrent in resp.arguments.torrents {
            if torrent.percent_done.unwrap_or(0.0) < 1.0 {
                continue;
            }
            let Some(raw_id) = torrent.id else {
                tracing::warn!("skipping torrent with missing id");
                continue;
            };
            let id = TorrentId(raw_id);
            let Some(name) = torrent.name else {
                tracing::warn!(torrent_id = id.0, "skipping torrent with missing name");
                continue;
            };
            let Some(download_dir) = torrent.download_dir else {
                tracing::warn!(
                    torrent_id = id.0,
                    "skipping torrent with missing download_dir"
                );
                continue;
            };
            let files = torrent
                .files
                .unwrap_or_default()
                .into_iter()
                .filter(|f| f.length > 0 && f.bytes_completed >= f.length)
                .map(|f| TorrentFile {
                    name: Utf8PathBuf::from(f.name),
                    length: f.length.max(0).cast_unsigned(),
                })
                .collect();
            result.push(TorrentInfo {
                id,
                name,
                download_dir: Utf8PathBuf::from(download_dir),
                files,
            });
        }
        Ok(result)
    }

    #[tracing::instrument(skip(self), fields(torrent_id = id.0))]
    pub async fn fetch(&mut self, id: TorrentId) -> AnyResult<TorrentInfo> {
        let fields = vec![
            TorrentGetField::Id,
            TorrentGetField::Name,
            TorrentGetField::DownloadDir,
            TorrentGetField::Files,
        ];

        let mut resp = self
            .inner
            .torrent_get(Some(fields), Some(vec![Id::Id(id.0)]))
            .await
            .map_err(|e| eyre::eyre!("transmission RPC request failed: {e}"))?;

        let torrent = resp
            .arguments
            .torrents
            .pop()
            .ok_or_else(|| eyre::eyre!("torrent {id} not found", id = id.0))?;

        let name = torrent
            .name
            .ok_or_else(|| eyre::eyre!("torrent {id} missing name", id = id.0))?;

        let download_dir = torrent
            .download_dir
            .ok_or_else(|| eyre::eyre!("torrent {id} missing download_dir", id = id.0))?;

        let files = torrent
            .files
            .unwrap_or_default()
            .into_iter()
            .filter(|f| f.length > 0 && f.bytes_completed >= f.length)
            .map(|f| TorrentFile {
                name: Utf8PathBuf::from(f.name),
                length: f.length.max(0).cast_unsigned(),
            })
            .collect();

        Ok(TorrentInfo {
            id,
            name,
            download_dir: Utf8PathBuf::from(download_dir),
            files,
        })
    }
}
