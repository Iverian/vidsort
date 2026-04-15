#![warn(clippy::pedantic)]

use std::process::ExitCode;

use clap::Parser;
use vidsort::classifier;
use vidsort::classifier::ClassificationKind;
use vidsort::config::TracingConfig;
use vidsort::config::TransmissionConfig;
use vidsort::config::TvdbConfig;
use vidsort::linker;
use vidsort::report::AnyResult;
use vidsort::transmission;
use vidsort::tvdb;

#[derive(Parser, Debug)]
#[command(about = "List all completed torrents and show how vidsort would classify them")]
struct Config {
    #[command(flatten)]
    transmission: TransmissionConfig,

    #[command(flatten)]
    tvdb: TvdbConfig,

    #[command(flatten)]
    tracing: TracingConfig,

    #[arg(short = 'm', long, action)]
    show_only_matched: bool,

    #[arg(short = 'd', long, action)]
    detailed: bool,

    #[arg(long, env = "VIDSORT_IMDB_BLACKLIST", value_delimiter = ',')]
    imdb_blacklist: Vec<String>,
}

fn main() -> ExitCode {
    dotenvy::dotenv().ok();
    let config = Config::parse();
    config.tracing.init();

    let result = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime")
        .block_on(run(config));

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:?}");
            ExitCode::FAILURE
        }
    }
}

#[allow(clippy::cast_precision_loss)] // display-only; file sizes never exceed f64 precision meaningfully
fn fmt_size(bytes: u64) -> String {
    human_bytes::human_bytes(bytes as f64)
}

async fn run(config: Config) -> AnyResult<()> {
    let mut client = transmission::Client::new(&config.transmission);
    let tvdb = tvdb::Client::new(&config.tvdb)?;
    let torrents = client.fetch_all_completed().await?;

    println!("{} completed torrent(s)\n", torrents.len());

    for info in torrents {
        let id = info.id.0;
        let name = info.name.clone();

        match classifier::classify(info, &tvdb, &config.imdb_blacklist).await {
            Ok(c) => match c.kind {
                ClassificationKind::Show(result) => {
                    println!("#{id}  {name:?}  →  show");
                    if config.detailed {
                        for show_file in &result.video_files {
                            println!(
                                "     \"{}\"  {}  →  \"{}\"",
                                show_file.file.name,
                                fmt_size(show_file.file.length),
                                linker::show_dest(&result.meta, show_file)
                            );
                        }
                    }
                    println!("     total:  {} file(s)", c.all_files.len());
                    println!();
                }
                ClassificationKind::Movie(result) => {
                    println!("#{id}  {name:?}  →  movie");
                    if config.detailed {
                        for file in &result.video_files {
                            println!(
                                "     \"{}\"  {}  →  \"{}\"",
                                file.name,
                                fmt_size(file.length),
                                linker::movie_dest(&result.meta, file)
                            );
                        }
                    }
                    println!("     total:  {} file(s)", c.all_files.len());
                    println!();
                }
                ClassificationKind::Other if !config.show_only_matched => {
                    println!("#{id}  {name:?}  →  other");
                    if config.detailed {
                        for file in &c.all_files {
                            println!("     \"{}\"  {}", file.name, fmt_size(file.length));
                        }
                    }
                    println!("     total:   {} file(s)", c.all_files.len());
                    println!();
                }
                ClassificationKind::Other => {}
            },
            Err(e) if !config.show_only_matched => {
                println!("#{id}  {name:?}  →  other  (classification error)");
                println!("     error:   {e}");
                println!();
            }
            Err(_) => {}
        }
    }

    tvdb.flush().await;
    Ok(())
}
