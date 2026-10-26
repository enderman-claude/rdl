mod download;
mod preset;
mod probe;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{HeaderName, HeaderValue};

use download::{Concurrency, Job};
use probe::Probe;

const MIB: u64 = 1024 * 1024;

/// Files below this size skip auto-tuning: the ramp would cost more than the download.
const AUTO_MIN_BYTES: u64 = 8 * MIB;
const DEFAULT_CONNECTIONS: usize = 8;

#[derive(Parser, Debug)]
#[command(name = "rdl", version, about = "Fast single-file downloader")]
struct Cli {
    /// URL of the file to download
    url: String,

    /// Output file path (default: the file name from the URL)
    #[arg(short, long)]
    output: Option<PathBuf>,

    /// Header preset: chrome (default), curl, or none (same as curl)
    #[arg(long, default_value = "chrome")]
    preset: String,

    /// Pin the number of parallel connections (disables auto-tuning)
    #[arg(short = 'c', long)]
    connections: Option<usize>,

    /// Chunk size in MiB for parallel range requests
    #[arg(long, default_value_t = 4)]
    chunk: u64,

    /// Skip auto-tuning and use fixed defaults
    #[arg(long)]
    no_probe: bool,

    /// Extra header as "Name: value" (repeatable, overrides the preset)
    #[arg(short = 'H', long = "header", value_name = "K:V")]
    headers: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    run(Cli::parse()).await
}

async fn run(cli: Cli) -> Result<()> {
    let url = url::Url::parse(&cli.url).context("invalid URL")?;
    if !matches!(url.scheme(), "http" | "https") {
        bail!("only http and https URLs are supported");
    }
    if cli.chunk == 0 {
        bail!("--chunk must be at least 1 MiB");
    }
    if cli.connections == Some(0) {
        bail!("-c must be at least 1");
    }

    let mut headers = preset::build(&cli.preset)?;
    for raw in &cli.headers {
        let (name, value) = raw
            .split_once(':')
            .with_context(|| format!("header '{raw}' must look like 'Name: value'"))?;
        headers.insert(
            HeaderName::from_bytes(name.trim().as_bytes())
                .with_context(|| format!("invalid header name in '{raw}'"))?,
            HeaderValue::from_str(value.trim())
                .with_context(|| format!("invalid header value in '{raw}'"))?,
        );
    }

    let client = reqwest::Client::builder()
        .default_headers(headers)
        .connect_timeout(Duration::from_secs(15))
        .pool_max_idle_per_host(64)
        .build()?;

    let output = cli
        .output
        .clone()
        .unwrap_or_else(|| PathBuf::from(file_name_from_url(&url)));

    let bar = ProgressBar::new(0);
    bar.set_style(
        ProgressStyle::with_template(
            "{spinner} [{bar:40}] {bytes}/{total_bytes} {bytes_per_sec} eta {eta}",
        )?
        .progress_chars("=> "),
    );

    match probe::probe(&client, &url).await? {
        Probe::Stream(resp) => {
            bar.set_length(resp.content_length().unwrap_or(0));
            download::stream(resp, &output, &bar).await?;
        }
        Probe::Ranged { total } => {
            bar.set_length(total);
            let concurrency = match (cli.connections, cli.no_probe) {
                (Some(n), _) => Concurrency::Fixed(n),
                (None, true) => Concurrency::Fixed(DEFAULT_CONNECTIONS),
                (None, false) if total < AUTO_MIN_BYTES => Concurrency::Fixed(2),
                (None, false) => Concurrency::Auto,
            };
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(true)
                .open(&output)
                .with_context(|| format!("creating {}", output.display()))?;
            file.set_len(total)?;

            let job = Job {
                client,
                url,
                file,
                total,
                chunk: cli.chunk * MIB,
                concurrency,
            };
            download::ranged(job, &bar).await?;
        }
    }

    let bytes = bar.position();
    bar.finish_and_clear();
    println!("saved {} ({bytes} bytes)", output.display());
    Ok(())
}

/// Last path segment of the URL, percent-decoded and made safe as a file name.
fn file_name_from_url(url: &url::Url) -> String {
    url.path_segments()
        .and_then(|segments| segments.last())
        .map(|s| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8_lossy()
                .into_owned()
        })
        .map(|s| s.replace(|c| c == '/' || c == '\\', "_"))
        .filter(|s| !s.is_empty() && s != "." && s != "..")
        .unwrap_or_else(|| "download".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_name_comes_from_last_segment() {
        let url = url::Url::parse("https://example.com/files/linux%20image.iso?x=1").unwrap();
        assert_eq!(file_name_from_url(&url), "linux image.iso");
    }

    #[test]
    fn file_name_falls_back_when_empty() {
        let url = url::Url::parse("https://example.com/").unwrap();
        assert_eq!(file_name_from_url(&url), "download");
    }

    #[test]
    fn file_name_cannot_escape_directory() {
        let url = url::Url::parse("https://example.com/..%2F..%2Fetc").unwrap();
        assert!(!file_name_from_url(&url).contains('/'));
    }
}
