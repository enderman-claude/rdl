//! Header presets. A preset defines the whole header set it sends.
//!
//! - `chrome` (default): a Chrome header set, with the Chrome version and OS detected locally.
//! - `curl` / `none`: what curl itself sends, using the locally installed curl version.

use std::env;
use std::process::Command;

use anyhow::{bail, Result};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue, ACCEPT, USER_AGENT};

const FALLBACK_CHROME: &str = "131.0.6778.85";
const FALLBACK_CURL: &str = "8.5.0";

pub fn build(name: &str) -> Result<HeaderMap> {
    match name {
        "chrome" | "chrome-auto" => chrome(),
        "curl" | "none" => curl(),
        other => bail!("unknown preset '{other}' (expected chrome, curl, or none)"),
    }
}

fn curl() -> Result<HeaderMap> {
    let version = detect_curl_version().unwrap_or_else(|| FALLBACK_CURL.to_string());
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_str(&format!("curl/{version}"))?,
    );
    headers.insert(ACCEPT, HeaderValue::from_static("*/*"));
    Ok(headers)
}

fn chrome() -> Result<HeaderMap> {
    let version = detect_chrome_version().unwrap_or_else(|| FALLBACK_CHROME.to_string());
    let major = version.split('.').next().unwrap_or("131").to_string();
    let (os_token, platform) = match env::consts::OS {
        "windows" => ("Windows NT 10.0; Win64; x64", "Windows"),
        "macos" => ("Macintosh; Intel Mac OS X 10_15_7", "macOS"),
        _ => ("X11; Linux x86_64", "Linux"),
    };
    let user_agent = format!(
        "Mozilla/5.0 ({os_token}) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/{version} Safari/537.36"
    );

    let mut headers = HeaderMap::new();
    set(&mut headers, "user-agent", &user_agent)?;
    set(
        &mut headers,
        "sec-ch-ua",
        &format!(
            "\"Google Chrome\";v=\"{major}\", \"Chromium\";v=\"{major}\", \"Not_A Brand\";v=\"24\""
        ),
    )?;
    set(&mut headers, "sec-ch-ua-mobile", "?0")?;
    set(
        &mut headers,
        "sec-ch-ua-platform",
        &format!("\"{platform}\""),
    )?;
    set(&mut headers, "accept", "*/*")?;
    set(&mut headers, "accept-language", &accept_language())?;
    // Compressed responses would break byte ranges, so always ask for raw bytes.
    set(&mut headers, "accept-encoding", "identity")?;
    set(&mut headers, "upgrade-insecure-requests", "1")?;
    set(&mut headers, "sec-fetch-site", "none")?;
    set(&mut headers, "sec-fetch-mode", "navigate")?;
    set(&mut headers, "sec-fetch-user", "?1")?;
    set(&mut headers, "sec-fetch-dest", "document")?;
    Ok(headers)
}

fn set(headers: &mut HeaderMap, name: &'static str, value: &str) -> Result<()> {
    headers.insert(HeaderName::from_static(name), HeaderValue::from_str(value)?);
    Ok(())
}

/// Derives an Accept-Language value from LC_ALL / LANG, e.g. "en_AU.UTF-8" -> "en-AU".
fn accept_language() -> String {
    let raw = env::var("LC_ALL")
        .or_else(|_| env::var("LANG"))
        .unwrap_or_default();
    let tag = raw.split('.').next().unwrap_or("").replace('_', "-");
    if tag.is_empty() || tag == "C" || tag == "POSIX" {
        "en-US,en;q=0.9".to_string()
    } else {
        format!("{tag},en;q=0.9")
    }
}

fn run_tool(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// "curl 8.5.0 (x86_64-pc-linux-gnu) ..." -> "8.5.0"
fn detect_curl_version() -> Option<String> {
    let text = run_tool("curl", &["--version"])?;
    text.split_whitespace().nth(1).map(str::to_string)
}

/// Reads the installed Chrome (or Chromium) version from the platform's usual location.
fn detect_chrome_version() -> Option<String> {
    let text = match env::consts::OS {
        "windows" => run_tool(
            "reg",
            &[
                "query",
                r"HKCU\Software\Google\Chrome\BLBeacon",
                "/v",
                "version",
            ],
        ),
        "macos" => run_tool(
            "defaults",
            &[
                "read",
                "/Applications/Google Chrome.app/Contents/Info",
                "CFBundleShortVersionString",
            ],
        ),
        _ => run_tool("google-chrome", &["--version"])
            .or_else(|| run_tool("chromium", &["--version"]))
            .or_else(|| run_tool("chromium-browser", &["--version"])),
    }?;

    text.split_whitespace()
        .find(|token| token.contains('.') && token.starts_with(|c: char| c.is_ascii_digit()))
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_preset_is_rejected() {
        assert!(build("firefox").is_err());
    }

    #[test]
    fn chrome_requests_raw_bytes() {
        let headers = build("chrome").unwrap();
        assert_eq!(headers.get("accept-encoding").unwrap(), "identity");
        assert!(headers
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Chrome/"));
    }

    #[test]
    fn curl_sends_only_user_agent_and_accept() {
        let headers = build("none").unwrap();
        assert_eq!(headers.len(), 2);
        assert!(headers["user-agent"].to_str().unwrap().starts_with("curl/"));
        assert_eq!(headers["accept"], "*/*");
    }
}
