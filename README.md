# rdl

A fast single-file downloader. Give it a direct URL and it downloads the file over parallel
HTTP range requests, tuning the number of connections automatically.

## Usage

```
rdl <URL> [-o out] [--preset chrome|curl|none] [-c N] [--chunk MiB] [--no-probe] [-H "Name: value"]...
```

| Flag | Meaning |
| --- | --- |
| `-o, --output` | Output path. Default: the file name from the URL. |
| `--preset` | Header set to send. Default `chrome`. |
| `-c, --connections` | Pin the number of parallel connections. Disables auto-tuning. |
| `--chunk` | Chunk size in MiB for range requests. Default `4`. |
| `--no-probe` | Skip auto-tuning and use 8 connections. |
| `-H, --header` | Add or override a header. Repeatable. |

## Presets

A preset defines the whole header set.

- **`chrome`** (default): a Chrome header set. The Chrome version is read from the locally
  installed browser, and the OS and language are derived from the environment. If no
  browser is found, a built-in recent version is used.
- **`curl`** (or `none`): what curl sends, using the locally installed curl version.
  Only `User-Agent` and `Accept` are sent.

`-H` is applied on top of whichever preset is active.

## Auto-tuning

For files of 8 MiB or more, with no `-c`, `rdl` starts with 2 connections and adds more
each second while throughput improves by at least 10%. The ramp stops after 5 seconds at
most, or at 32 connections. Every byte fetched during the ramp is part of the download.

## Limits

- Headers only. TLS and HTTP/2 fingerprints are those of the Rust client, so sites that check
  TLS fingerprints will still see a non-browser client.
- No resume after interruption yet. A failed run leaves a partial file behind.
- No rate limiting yet.
- Progress updates when each chunk completes, so granularity is one chunk (4 MiB by default).

## Building

```
cargo build --release
```

Binaries are built for Linux, macOS, and Windows by the CI workflow in
`.github/workflows/ci.yml`, and are attached to each run as artifacts.
