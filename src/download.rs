//! Download engine.
//!
//! Ranged downloads use a shared chunk queue. Workers pull the next chunk index,
//! fetch it with a `Range` request, and write it at its offset in a preallocated
//! file. In auto mode the controller starts with a few workers and adds more
//! every second while throughput keeps improving, for at most `AUTO_BUDGET`.
//! Every byte the probe fetches is real download work, so nothing is wasted.

use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use indicatif::ProgressBar;
use reqwest::header::RANGE;
use reqwest::{Client, Response, StatusCode};
use tokio::io::AsyncWriteExt;
use tokio::task::JoinSet;
use url::Url;

const AUTO_START: usize = 2;
const AUTO_MAX: usize = 32;
const AUTO_BUDGET: Duration = Duration::from_secs(5);
/// A step must lift throughput by at least this factor to be worth keeping.
const AUTO_GAIN: f64 = 1.10;
const MAX_ATTEMPTS: u32 = 5;

#[derive(Clone, Copy, Debug)]
pub enum Concurrency {
    Fixed(usize),
    Auto,
}

pub struct Job {
    pub client: Client,
    pub url: Url,
    pub file: File,
    pub total: u64,
    pub chunk: u64,
    pub concurrency: Concurrency,
}

struct Shared {
    client: Client,
    url: Url,
    file: File,
    total: u64,
    chunk: u64,
    n_chunks: u64,
    next: AtomicU64,
    done: AtomicU64,
}

/// Sequential download for servers without range support.
pub async fn stream(resp: Response, path: &Path, bar: &ProgressBar) -> Result<()> {
    let mut out = tokio::fs::File::create(path)
        .await
        .with_context(|| format!("creating {}", path.display()))?;
    let mut body = resp.bytes_stream();
    while let Some(part) = body.next().await {
        let part = part?;
        out.write_all(&part).await?;
        bar.inc(part.len() as u64);
    }
    out.flush().await?;
    Ok(())
}

/// Parallel range download into a preallocated file.
pub async fn ranged(job: Job, bar: &ProgressBar) -> Result<()> {
    let Job {
        client,
        url,
        file,
        total,
        chunk,
        concurrency,
    } = job;
    let n_chunks = total.div_ceil(chunk);
    let shared = Arc::new(Shared {
        client,
        url,
        file,
        total,
        chunk,
        n_chunks,
        next: AtomicU64::new(0),
        done: AtomicU64::new(0),
    });

    let (initial, auto) = match concurrency {
        Concurrency::Fixed(n) => (n, false),
        Concurrency::Auto => (AUTO_START, true),
    };
    let mut active = initial.min(n_chunks as usize).max(1);
    let mut set = JoinSet::new();
    for _ in 0..active {
        set.spawn(worker(Arc::clone(&shared)));
    }

    let started = Instant::now();
    let mut ticker = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(1),
        Duration::from_secs(1),
    );
    let mut ramping = auto;
    let mut prev_rate: Option<f64> = None;
    let mut last_done = 0u64;

    loop {
        tokio::select! {
            joined = set.join_next(), if !set.is_empty() => {
                match joined {
                    Some(Ok(Ok(()))) | None => {}
                    Some(Ok(Err(err))) => {
                        set.abort_all();
                        return Err(err);
                    }
                    Some(Err(err)) => {
                        set.abort_all();
                        return Err(err.into());
                    }
                }
                if set.is_empty() {
                    break;
                }
            }
            _ = ticker.tick() => {
                let done = shared.done.load(Ordering::Relaxed);
                bar.set_position(done);
                if ramping {
                    // Bytes gained over the last second, which reflects the current worker count.
                    let rate = (done - last_done) as f64;
                    last_done = done;
                    if prev_rate.is_some_and(|prev| rate < prev * AUTO_GAIN) {
                        ramping = false;
                    }
                    prev_rate = Some(rate);

                    let queue_left = shared.next.load(Ordering::Relaxed) < n_chunks;
                    if ramping && started.elapsed() < AUTO_BUDGET && active < AUTO_MAX && queue_left {
                        let step = active.min(AUTO_MAX - active);
                        for _ in 0..step {
                            set.spawn(worker(Arc::clone(&shared)));
                        }
                        active += step;
                    } else {
                        ramping = false;
                    }
                }
            }
        }
    }

    let done = shared.done.load(Ordering::Relaxed);
    bar.set_position(done);
    if done != total {
        bail!("download incomplete: {done} of {total} bytes");
    }
    Ok(())
}

async fn worker(shared: Arc<Shared>) -> Result<()> {
    loop {
        let index = shared.next.fetch_add(1, Ordering::Relaxed);
        if index >= shared.n_chunks {
            return Ok(());
        }
        fetch_chunk(&shared, index).await?;
    }
}

async fn fetch_chunk(shared: &Arc<Shared>, index: u64) -> Result<()> {
    let start = index * shared.chunk;
    let end = (start + shared.chunk).min(shared.total) - 1;

    let mut attempt = 0;
    loop {
        attempt += 1;
        match try_chunk(shared, start, end).await {
            Ok(()) => return Ok(()),
            Err(_) if attempt < MAX_ATTEMPTS => {
                tokio::time::sleep(backoff(attempt)).await;
            }
            Err(err) => {
                return Err(err.context(format!("chunk {index} failed after {attempt} attempts")))
            }
        }
    }
}

async fn try_chunk(shared: &Arc<Shared>, start: u64, end: u64) -> Result<()> {
    let resp = shared
        .client
        .get(shared.url.clone())
        .header(RANGE, format!("bytes={start}-{end}"))
        .send()
        .await?;
    if resp.status() != StatusCode::PARTIAL_CONTENT {
        bail!("expected 206 for range request, got {}", resp.status());
    }

    let body = resp.bytes().await?;
    let expected = end - start + 1;
    if body.len() as u64 != expected {
        bail!("short chunk: got {} of {expected} bytes", body.len());
    }

    let owner = Arc::clone(shared);
    tokio::task::spawn_blocking(move || write_all_at(&owner.file, &body, start)).await??;
    shared.done.fetch_add(expected, Ordering::Relaxed);
    Ok(())
}

/// Exponential backoff with a little jitter, between 0.5 s and about 8 s.
fn backoff(attempt: u32) -> Duration {
    let base = (250u64 * (1u64 << attempt.min(5))).min(8000);
    let jitter = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::from(d.subsec_millis()) % 250)
        .unwrap_or(0);
    Duration::from_millis(base + jitter)
}

#[cfg(unix)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    while !buf.is_empty() {
        let written = file.write_at(buf, offset)?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[written..];
        offset += written as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &File, mut buf: &[u8], mut offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let written = file.seek_write(buf, offset)?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        buf = &buf[written..];
        offset += written as u64;
    }
    Ok(())
}
