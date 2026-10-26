//! Initial request: learns the file size and whether byte ranges work.
//!
//! A `Range: bytes=0-0` GET is used instead of HEAD, because many servers
//! answer HEAD incorrectly, while the Content-Range of a ranged GET is reliable.

use anyhow::{bail, Context, Result};
use reqwest::header::{CONTENT_RANGE, RANGE};
use reqwest::{Client, Response, StatusCode};
use url::Url;

pub enum Probe {
    /// The server honours byte ranges; `total` is the file size in bytes.
    Ranged { total: u64 },
    /// No range support: the response body is the whole file, so stream it.
    Stream(Response),
}

pub async fn probe(client: &Client, url: &Url) -> Result<Probe> {
    let resp = client
        .get(url.clone())
        .header(RANGE, "bytes=0-0")
        .send()
        .await
        .context("probe request failed")?;

    match resp.status() {
        StatusCode::PARTIAL_CONTENT => {
            let value = resp
                .headers()
                .get(CONTENT_RANGE)
                .context("206 response without Content-Range")?
                .to_str()?;
            Ok(Probe::Ranged {
                total: parse_total(value)?,
            })
        }
        status if status.is_success() => Ok(Probe::Stream(resp)),
        status => bail!("server returned HTTP {status}"),
    }
}

/// "bytes 0-0/12345" -> 12345
pub fn parse_total(content_range: &str) -> Result<u64> {
    let total = content_range.rsplit('/').next().unwrap_or_default().trim();
    total
        .parse::<u64>()
        .with_context(|| format!("unusable Content-Range '{content_range}'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_total_from_content_range() {
        assert_eq!(parse_total("bytes 0-0/12345").unwrap(), 12345);
    }

    #[test]
    fn unknown_total_is_an_error() {
        assert!(parse_total("bytes 0-0/*").is_err());
    }
}
