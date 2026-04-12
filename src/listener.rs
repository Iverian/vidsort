use std::os::fd::OwnedFd;

use camino::Utf8Path;
use camino::Utf8PathBuf;
use nix::fcntl::OFlag;
use nix::fcntl::open;
use nix::sys::stat::Mode;
use tokio::io::AsyncBufReadExt as _;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::report::AnyResult;
use crate::types::TorrentId;

#[tracing::instrument(skip(tx, ct), fields(path = %fifo_path))]
pub async fn run(
    fifo_path: Utf8PathBuf,
    tx: mpsc::Sender<TorrentId>,
    ct: CancellationToken,
) -> AnyResult<()> {
    ensure_fifo(&fifo_path)?;

    let read_fd = open_read(&fifo_path)?;
    // Hold a write end open to prevent the read side from seeing EOF when no
    // external writer (Transmission) is currently connected.
    let _write_guard = open_write(&fifo_path)?;

    let std_file = std::fs::File::from(read_fd);
    let tokio_file = tokio::fs::File::from_std(std_file);
    let mut lines = tokio::io::BufReader::new(tokio_file).lines();

    tracing::info!("listening on FIFO");

    loop {
        tokio::select! {
            biased;
            () = ct.cancelled() => break,
            result = lines.next_line() => {
                match result? {
                    None => {
                        // EOF — should not happen while write guard is held
                        tracing::warn!("FIFO reached EOF unexpectedly");
                        break;
                    }
                    Some(line) => dispatch_line(line, &tx).await?,
                }
            }
        }
    }

    Ok(())
}

async fn dispatch_line(line: String, tx: &mpsc::Sender<TorrentId>) -> AnyResult<()> {
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }
    if let Ok(id) = line.parse::<i64>() {
        tracing::debug!(torrent_id = id, "received torrent ID");
        if tx.send(TorrentId(id)).await.is_err() {
            return Err(eyre::eyre!("fetcher channel closed"));
        }
    } else {
        tracing::warn!(line = %line, "ignoring unparseable FIFO line");
    }
    Ok(())
}

fn ensure_fifo(path: &Utf8Path) -> AnyResult<()> {
    if !path.exists() {
        nix::unistd::mkfifo(path.as_std_path(), Mode::S_IRUSR | Mode::S_IWUSR)?;
        tracing::info!(path = %path, "created FIFO");
    }
    Ok(())
}

fn open_read(path: &Utf8Path) -> AnyResult<OwnedFd> {
    Ok(open(
        path.as_std_path(),
        OFlag::O_RDONLY | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?)
}

fn open_write(path: &Utf8Path) -> AnyResult<OwnedFd> {
    Ok(open(
        path.as_std_path(),
        OFlag::O_WRONLY | OFlag::O_NONBLOCK,
        Mode::empty(),
    )?)
}
