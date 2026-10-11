use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT, RANGE, USER_AGENT};
use reqwest::{Client, StatusCode};
use std::fmt;
use std::fs::File;
use std::io::{IsTerminal, Write};
use std::os::windows::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Debug)]
pub struct FetchError {
    pub not_found: bool,
    pub msg: String,
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.msg)
    }
}

impl std::error::Error for FetchError {}

fn check(resp: reqwest::Response, url: &str) -> std::result::Result<reqwest::Response, FetchError> {
    let status = resp.status();
    if status.is_success() || status == StatusCode::PARTIAL_CONTENT {
        return Ok(resp);
    }
    Err(FetchError {
        not_found: status == StatusCode::NOT_FOUND,
        msg: format!("{url}: HTTP {}", status.as_u16()),
    })
}

pub struct Http {
    client: Client,
    verbose: bool,
}

impl Http {
    pub fn new(verbose: bool) -> Result<Http> {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(concat!("wing/", env!("CARGO_PKG_VERSION"))),
        );
        let client = Client::builder()
            .user_agent(concat!("wing/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .tcp_nodelay(true)
            .pool_max_idle_per_host(8)
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(600))
            .build()
            .context("building HTTP client")?;
        Ok(Http { client, verbose })
    }

    pub async fn get_string(&self, url: &str) -> std::result::Result<String, FetchError> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| FetchError { not_found: false, msg: format!("{url}: {e}") })?;
        let resp = check(resp, url)?;
        resp.text()
            .await
            .map_err(|e| FetchError { not_found: false, msg: format!("{url}: {e}") })
    }

    pub async fn get_string_with_header(
        &self,
        url: &str,
        accept: &str,
    ) -> std::result::Result<String, FetchError> {
        let resp = self
            .client
            .get(url)
            .header(ACCEPT, accept)
            .send()
            .await
            .map_err(|e| FetchError { not_found: false, msg: format!("{url}: {e}") })?;
        let resp = check(resp, url)?;
        resp.text()
            .await
            .map_err(|e| FetchError { not_found: false, msg: format!("{url}: {e}") })
    }

    /// Downloads `url` to `dest`, using parallel byte ranges when the server
    /// supports them. Returns the number of bytes written.
    pub async fn download(&self, url: &str, dest: &Path) -> Result<u64> {
        let total = {
            let head = self
                .client
                .get(url)
                .header(RANGE, "bytes=0-0")
                .send()
                .await
                .with_context(|| format!("GET {url}"))?
                .error_for_status()
                .with_context(|| format!("GET {url}"))?;
            let h = head.headers().clone();
            let t = content_length(&h);
            let accept = h
                .get("accept-ranges")
                .and_then(|v| v.to_str().ok())
                .map(|v| v.eq_ignore_ascii_case("bytes"))
                .unwrap_or(false);
            if accept && t > 0 {
                Some(t)
            } else {
                None
            }
        };

        let Some(total) = total else {
            return self.download_single(url, dest).await;
        };

        let chunk_size: u64 = 8 * 1024 * 1024;
        let nchunks = ((total + chunk_size - 1) / chunk_size).clamp(1, 8) as usize;
        let file = Arc::new(
            File::create(dest).with_context(|| format!("creating {}", dest.display()))?,
        );
        file.set_len(total)?;

        let done = Arc::new(AtomicU64::new(0));
        let mut tasks = Vec::new();
        for i in 0..nchunks {
            let start = i as u64 * chunk_size;
            let end = (start + chunk_size - 1).min(total - 1);
            let client = self.client.clone();
            let url = url.to_string();
            let file = file.clone();
            let done = done.clone();
            let verbose = self.verbose && std::io::stderr().is_terminal();
            tasks.push(tokio::spawn(async move {
                let mut resp = client
                    .get(&url)
                    .header(RANGE, format!("bytes={start}-{end}"))
                    .send()
                    .await?
                    .error_for_status()?;
                let mut offset = start;
                while let Some(chunk) = resp.chunk().await? {
                    let mut rest = &chunk[..];
                    while !rest.is_empty() {
                        let n = rest.len().min(64 * 1024);
                        file.seek_write(&rest[..n], offset)?;
                        offset += n as u64;
                        done.fetch_add(n as u64, Ordering::Relaxed);
                        rest = &rest[n..];
                    }
                    if verbose {
                        let pct = done.load(Ordering::Relaxed) as f64 * 100.0 / total as f64;
                        eprint!("\r  downloading {pct:5.1}%  {:.1}/{:.1} MB",
                            done.load(Ordering::Relaxed) as f64 / 1e6,
                            total as f64 / 1e6);
                        let _ = std::io::stderr().flush();
                    }
                }
                Ok::<(), anyhow::Error>(())
            }));
        }
        let mut err = None;
        for t in tasks {
            if let Ok(Err(e)) = t.await {
                err = Some(e);
            }
        }
        if self.verbose && std::io::stderr().is_terminal() {
            eprintln!();
        }
        if let Some(e) = err {
            let _ = std::fs::remove_file(dest);
            return Err(e);
        }
        Ok(total)
    }

    async fn download_single(&self, url: &str, dest: &Path) -> Result<u64> {
        let mut file =
            File::create(dest).with_context(|| format!("creating {}", dest.display()))?;
        let mut resp = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()?;
        let mut written = 0u64;
        let total = resp.content_length().unwrap_or(0);
        let verbose = self.verbose && std::io::stderr().is_terminal();
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk)?;
            written += chunk.len() as u64;
            if verbose && total > 0 {
                let pct = written as f64 * 100.0 / total as f64;
                eprint!(
                    "\r  downloading {pct:5.1}%  {:.1}/{:.1} MB",
                    written as f64 / 1e6,
                    total as f64 / 1e6
                );
                let _ = std::io::stderr().flush();
            }
        }
        if verbose {
            eprintln!();
        }
        Ok(written)
    }
}

fn content_length(h: &HeaderMap) -> u64 {
    h.get("content-range")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.rsplit('/').next().and_then(|s| s.trim().parse::<u64>().ok()))
        .unwrap_or(0)
}

/// Local file helper used by the archive installer.
pub fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

