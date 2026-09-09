//! Video backend: delegate streaming sites to `yt-dlp`.
//!
//! fetchd's own HTTP engine downloads files. Adaptive-streaming sites
//! (YouTube DASH, HLS) are a different problem: separate audio/video streams,
//! signed short-lived segment URLs, a per-session cipher, then a mux. `yt-dlp`
//! solves all of that and supports ~1800 sites, so for those we shell out to it
//! rather than reinventing an extractor. Normal file downloads never touch this
//! module.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// Hosts routed to yt-dlp automatically. Not exhaustive — yt-dlp handles far
/// more — but these are the ones worth auto-detecting; anything else can still
/// be forced via the "Download video" action.
const VIDEO_HOSTS: &[&str] = &[
    "youtube.com", "youtu.be", "m.youtube.com", "music.youtube.com",
    "vimeo.com", "dailymotion.com", "twitch.tv", "clips.twitch.tv",
    "tiktok.com", "instagram.com", "facebook.com", "fb.watch",
    "twitter.com", "x.com", "reddit.com", "soundcloud.com",
    "bilibili.com", "nicovideo.jp", "streamable.com",
];

pub fn is_video_site(url: &str) -> bool {
    let host = match reqwest::Url::parse(url) {
        Ok(u) => u.host_str().unwrap_or("").to_ascii_lowercase(),
        Err(_) => return false,
    };
    VIDEO_HOSTS
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{h}")))
}

/// Where cookies come from for age/region-locked or members-only videos.
#[derive(Debug, Clone, Default)]
pub struct Cookies {
    /// Netscape `cookies.txt` path → `--cookies`.
    pub file: Option<PathBuf>,
    /// Browser name → `--cookies-from-browser` (reads the live jar directly).
    pub browser: Option<String>,
}

/// One progress tick parsed from yt-dlp. Bytes are for the *current* stream
/// (yt-dlp downloads video then audio), so totals reset between phases. Speed
/// is derived on the frontend from the byte deltas, so it is not carried here.
#[derive(Debug, Clone, Copy)]
pub struct Tick {
    pub downloaded: u64,
    pub total: Option<u64>,
}

/// Format-selection args for a quality choice: "best" (default), a max height
/// like "1080", or "audio" (extract to mp3).
///
/// `height<=?H` is non-strict — if nothing matches exactly, yt-dlp still picks
/// the closest rather than failing. Video+audio (`bv*+ba`) is merged by ffmpeg;
/// `/b` is the fallback for progressive-only streams.
fn format_args(quality: &str) -> Vec<String> {
    match quality {
        "" | "best" => vec![],
        "audio" => vec![
            "-f".into(), "bestaudio/best".into(),
            "-x".into(), "--audio-format".into(), "mp3".into(),
        ],
        h => vec![
            "-f".into(),
            format!("bv*[height<=?{h}]+ba/b[height<=?{h}]"),
        ],
    }
}

/// Run yt-dlp for `url`, saving into `dir`. Returns the final file path.
#[allow(clippy::too_many_arguments)]
pub async fn run<F>(
    ytdlp: &str,
    url: &str,
    dir: &Path,
    cookies: &Cookies,
    quality: &str,
    token: CancellationToken,
    on_progress: F,
) -> Result<PathBuf, String>
where
    F: Fn(Tick) + Send + 'static,
{
    tokio::fs::create_dir_all(dir)
        .await
        .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;

    let out_tmpl = format!("{}/%(title)s [%(id)s].%(ext)s", dir.display());

    let mut cmd = Command::new(ytdlp);
    cmd.arg("--newline")
        .arg("--no-playlist")
        .arg("-o")
        .arg(&out_tmpl)
        // Pipe-delimited machine progress on stdout. NA where a field is
        // absent. (Do NOT add --print here: it silently suppresses
        // --progress-template. The final path is parsed from the Destination /
        // Merger lines instead.)
        .arg("--progress-template")
        .arg("download:FDPROG|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s")
        .arg("--no-warnings");

    for a in format_args(quality) {
        cmd.arg(a);
    }

    match (&cookies.file, &cookies.browser) {
        (Some(path), _) => {
            cmd.arg("--cookies").arg(path);
        }
        (None, Some(browser)) if !browser.is_empty() => {
            cmd.arg("--cookies-from-browser").arg(browser);
        }
        _ => {}
    }

    cmd.arg(url);
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    cmd.kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start yt-dlp: {e} (is it installed?)"))?;

    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    // Drain stderr concurrently so the pipe never blocks; keep the tail for the
    // error message.
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut tail = Vec::new();
        while let Ok(Some(line)) = lines.next_line().await {
            tail.push(line);
            if tail.len() > 20 {
                tail.remove(0);
            }
        }
        tail.join("\n")
    });

    // The final path is learned from yt-dlp's own messages. A merge or audio
    // extraction produces the true output and overrides the per-stream
    // download destination.
    let mut destination: Option<PathBuf> = None;
    let mut merged: Option<PathBuf> = None;
    let mut reader = BufReader::new(stdout).lines();

    loop {
        tokio::select! {
            _ = token.cancelled() => {
                let _ = child.kill().await;
                return Err("cancelled".into());
            }
            line = reader.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        if let Some(rest) = line.strip_prefix("FDPROG|") {
                            if let Some(tick) = parse_progress(rest) {
                                on_progress(tick);
                            }
                        } else if let Some(p) = parse_destination(&line) {
                            destination = Some(p);
                        } else if let Some(p) = parse_final(&line) {
                            merged = Some(p);
                        }
                    }
                    Ok(None) => break, // EOF
                    Err(e) => return Err(format!("yt-dlp output error: {e}")),
                }
            }
        }
    }
    let final_path = merged.or(destination);

    let status = child.wait().await.map_err(|e| format!("yt-dlp failed: {e}"))?;
    if !status.success() {
        let tail = stderr_task.await.unwrap_or_default();
        let msg = tail.lines().rev().find(|l| l.contains("ERROR")).unwrap_or(&tail);
        return Err(format!("yt-dlp failed: {}", msg.trim()));
    }

    // On success yt-dlp printed the path; fall back to the directory so the
    // "open folder" action still works if the print was somehow missed.
    Ok(final_path.unwrap_or_else(|| dir.to_path_buf()))
}

/// A per-stream download target: `[download] Destination: PATH`, or a
/// `... has already been downloaded` line.
fn parse_destination(line: &str) -> Option<PathBuf> {
    if let Some(p) = line.strip_prefix("[download] Destination: ") {
        return Some(PathBuf::from(p.trim()));
    }
    if let Some(rest) = line.strip_prefix("[download] ") {
        if let Some(p) = rest.strip_suffix(" has already been downloaded") {
            return Some(PathBuf::from(p.trim()));
        }
    }
    None
}

/// The real output after a merge or audio extraction, which supersedes the
/// per-stream destination.
fn parse_final(line: &str) -> Option<PathBuf> {
    // [Merger] Merging formats into "PATH"
    if let Some(i) = line.find("Merging formats into \"") {
        let rest = &line[i + "Merging formats into \"".len()..];
        if let Some(end) = rest.rfind('"') {
            return Some(PathBuf::from(&rest[..end]));
        }
    }
    // [ExtractAudio] Destination: PATH
    if let Some(p) = line.strip_prefix("[ExtractAudio] Destination: ") {
        return Some(PathBuf::from(p.trim()));
    }
    None
}

/// Parse `downloaded|total|estimate|speed`, each a number or "NA". Total falls
/// back to the estimate when the exact size is not yet known.
fn parse_progress(s: &str) -> Option<Tick> {
    let mut it = s.split('|');
    let downloaded = num_u64(it.next()?)?;
    let total_field = it.next().unwrap_or("NA");
    let est_field = it.next().unwrap_or("NA");
    let total = num_u64(total_field).or_else(|| num_u64(est_field));
    Some(Tick { downloaded, total })
}

fn num_u64(s: &str) -> Option<u64> {
    let s = s.trim();
    if s == "NA" || s.is_empty() {
        return None;
    }
    // yt-dlp emits floats for byte fields sometimes (e.g. "1048576.0").
    s.parse::<f64>().ok().map(|f| f as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_video_hosts() {
        assert!(is_video_site("https://www.youtube.com/watch?v=abc"));
        assert!(is_video_site("https://youtu.be/abc"));
        assert!(is_video_site("https://vimeo.com/12345"));
        assert!(is_video_site("https://clips.twitch.tv/foo"));
        assert!(!is_video_site("https://example.com/file.zip"));
        assert!(!is_video_site("https://notyoutube.com.evil.test/x"));
        assert!(!is_video_site("not a url"));
    }

    #[test]
    fn format_args_map_quality() {
        assert!(format_args("best").is_empty());
        assert!(format_args("").is_empty());
        assert_eq!(
            format_args("1080"),
            vec!["-f", "bv*[height<=?1080]+ba/b[height<=?1080]"]
        );
        let audio = format_args("audio");
        assert!(audio.contains(&"--audio-format".to_string()));
        assert!(audio.contains(&"mp3".to_string()));
    }

    #[test]
    fn parses_progress_lines() {
        let t = parse_progress("1048576|10485760|NA|524288.0").unwrap();
        assert_eq!(t.downloaded, 1_048_576);
        assert_eq!(t.total, Some(10_485_760));

        // Total absent, estimate present.
        let t = parse_progress("2048|NA|4096|1000").unwrap();
        assert_eq!(t.downloaded, 2048);
        assert_eq!(t.total, Some(4096));

        // Everything unknown but downloaded.
        let t = parse_progress("500|NA|NA|NA").unwrap();
        assert_eq!(t.downloaded, 500);
        assert_eq!(t.total, None);
    }

    #[test]
    fn parses_output_paths() {
        assert_eq!(
            parse_destination("[download] Destination: /d/Video.f137.mp4").unwrap(),
            PathBuf::from("/d/Video.f137.mp4")
        );
        assert_eq!(
            parse_destination("[download] /d/Video.mp4 has already been downloaded").unwrap(),
            PathBuf::from("/d/Video.mp4")
        );
        assert_eq!(
            parse_final("[Merger] Merging formats into \"/d/Video.mp4\"").unwrap(),
            PathBuf::from("/d/Video.mp4")
        );
        assert_eq!(
            parse_final("[ExtractAudio] Destination: /d/Song.mp3").unwrap(),
            PathBuf::from("/d/Song.mp3")
        );
        assert!(parse_destination("[download] 5.0% of 10MiB").is_none());
    }
}
