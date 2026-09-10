//! Netscape `cookies.txt` support.
//!
//! Some hosts sit behind an interactive anti-bot challenge (Cloudflare's is
//! the common one). No header combination gets past it: the server wants a
//! browser to execute a script, and the reward is a cookie such as
//! `cf_clearance`. Once a browser has earned that cookie, replaying it is
//! enough — so spool does not need to solve challenges, only to be handed
//! the result.
//!
//! The Netscape format is the lingua franca here: every "export cookies"
//! browser extension writes it, as does `yt-dlp --cookies-from-browser`. It is
//! also plain text, which is why this module needs no dependencies. Reading
//! Chromium's own cookie database instead would mean AES-decrypting it with a
//! key from the system keyring.
//!
//! Caveat worth knowing: a `cf_clearance` cookie is bound to the IP address
//! *and* the exact `User-Agent` that earned it, so the agent configured in
//! settings must match the browser the cookies came from.

use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub struct Cookie {
    pub domain: String,
    /// Whether the cookie also applies to subdomains of `domain`.
    pub include_subdomains: bool,
    pub path: String,
    pub secure: bool,
    /// Unix seconds; `0` means a session cookie with no expiry.
    pub expires: u64,
    pub name: String,
    pub value: String,
}

impl Cookie {
    fn matches(&self, host: &str, path: &str, https: bool, now: u64) -> bool {
        if self.secure && !https {
            return false;
        }
        if self.expires != 0 && self.expires <= now {
            return false;
        }
        if !domain_matches(&self.domain, self.include_subdomains, host) {
            return false;
        }
        path_matches(&self.path, path)
    }
}

/// `.example.com` with the subdomain flag covers `example.com` and any host
/// under it; without the flag only an exact host match counts.
fn domain_matches(cookie_domain: &str, include_subdomains: bool, host: &str) -> bool {
    let cookie_domain = cookie_domain.trim_start_matches('.').to_ascii_lowercase();
    let host = host.to_ascii_lowercase();

    if host == cookie_domain {
        return true;
    }
    include_subdomains && host.ends_with(&format!(".{cookie_domain}"))
}

/// A cookie path matches the request path if it is a prefix ending on a
/// segment boundary, so `/files` covers `/files/a.zip` but not `/filestore`.
fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    if cookie_path.is_empty() || cookie_path == "/" {
        return true;
    }
    if !request_path.starts_with(cookie_path) {
        return false;
    }
    let rest = &request_path[cookie_path.len()..];
    rest.is_empty() || rest.starts_with('/') || cookie_path.ends_with('/')
}

/// Parse a Netscape `cookies.txt` file.
///
/// Malformed lines are skipped rather than failing the whole file: these files
/// are produced by a zoo of browser extensions and one odd line should not
/// cost the user every other cookie.
pub fn parse(contents: &str) -> Vec<Cookie> {
    let mut cookies = Vec::new();

    for line in contents.lines() {
        let line = line.trim_end_matches(['\r', '\n']);

        // `#HttpOnly_` is a real prefix, not a comment; everything else
        // starting with `#` is.
        let line = match line.strip_prefix("#HttpOnly_") {
            Some(rest) => rest,
            None if line.starts_with('#') || line.trim().is_empty() => continue,
            None => line,
        };

        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 7 {
            continue;
        }

        let Ok(expires) = fields[4].trim().parse::<f64>() else {
            continue;
        };
        if fields[5].is_empty() {
            continue;
        }

        cookies.push(Cookie {
            domain: fields[0].to_string(),
            include_subdomains: fields[1].eq_ignore_ascii_case("TRUE"),
            path: fields[2].to_string(),
            secure: fields[3].eq_ignore_ascii_case("TRUE"),
            expires: expires.max(0.0) as u64,
            name: fields[5].to_string(),
            value: fields[6].to_string(),
        });
    }

    cookies
}

pub fn load(path: &Path) -> Result<Vec<Cookie>, String> {
    let body = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(parse(&body))
}

/// Build a `Cookie:` header value for one URL, or `None` if nothing matches.
pub fn header_for(cookies: &[Cookie], url: &reqwest::Url) -> Option<String> {
    let host = url.host_str()?;
    let path = url.path();
    let https = url.scheme() == "https";
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let pairs: Vec<String> = cookies
        .iter()
        .filter(|c| c.matches(host, path, https, now))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect();

    if pairs.is_empty() {
        None
    } else {
        Some(pairs.join("; "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Url;

    const SAMPLE: &str = "\
# Netscape HTTP Cookie File
# This is a comment

.example.com\tTRUE\t/\tTRUE\t2000000000\tcf_clearance\tabc123
example.com\tFALSE\t/files\tFALSE\t2000000000\tscoped\tyes
.example.com\tTRUE\t/\tFALSE\t1\texpired\tnope
#HttpOnly_.example.com\tTRUE\t/\tFALSE\t0\tsession\tkeep
malformed line without tabs
.other.com\tTRUE\t/\tFALSE\t2000000000\telsewhere\tno
";

    #[test]
    fn parses_and_skips_junk() {
        let cookies = parse(SAMPLE);
        let names: Vec<&str> = cookies.iter().map(|c| c.name.as_str()).collect();
        // The malformed line and both comments are gone; #HttpOnly_ is kept.
        assert_eq!(
            names,
            vec!["cf_clearance", "scoped", "expired", "session", "elsewhere"]
        );

        let cf = &cookies[0];
        assert!(cf.secure);
        assert!(cf.include_subdomains);
        assert_eq!(cf.value, "abc123");
        // A session cookie records 0, not an expiry in the past.
        assert_eq!(cookies[3].expires, 0);
    }

    #[test]
    fn sends_only_matching_cookies() {
        let cookies = parse(SAMPLE);
        let header =
            header_for(&cookies, &Url::parse("https://www.example.com/files/a.zip").unwrap())
                .unwrap();

        assert!(header.contains("cf_clearance=abc123"), "{header}");
        // Session cookie with no expiry still applies.
        assert!(header.contains("session=keep"), "{header}");
        // Expired.
        assert!(!header.contains("expired"), "{header}");
        // Different site entirely — leaking this would be a privacy bug.
        assert!(!header.contains("elsewhere"), "{header}");
        // Host-only cookie does not apply to the `www.` subdomain.
        assert!(!header.contains("scoped"), "{header}");
    }

    #[test]
    fn host_only_cookie_matches_exact_host_and_path() {
        let cookies = parse(SAMPLE);
        let header =
            header_for(&cookies, &Url::parse("https://example.com/files/a.zip").unwrap()).unwrap();
        assert!(header.contains("scoped=yes"), "{header}");

        // Same host, path outside the cookie's scope.
        let other =
            header_for(&cookies, &Url::parse("https://example.com/other").unwrap()).unwrap();
        assert!(!other.contains("scoped"), "{other}");
    }

    #[test]
    fn secure_cookies_never_go_over_plain_http() {
        let cookies = parse(SAMPLE);
        let header = header_for(&cookies, &Url::parse("http://example.com/").unwrap());
        let header = header.unwrap_or_default();
        assert!(!header.contains("cf_clearance"), "{header}");
    }

    #[test]
    fn unrelated_host_gets_nothing() {
        let cookies = parse(SAMPLE);
        assert_eq!(
            header_for(&cookies, &Url::parse("https://unrelated.test/f").unwrap()),
            None
        );
    }

    #[test]
    fn path_prefix_must_end_on_a_boundary() {
        assert!(path_matches("/files", "/files"));
        assert!(path_matches("/files", "/files/a.zip"));
        assert!(path_matches("/files/", "/files/a.zip"));
        // `/filestore` is a different directory, not a child of `/files`.
        assert!(!path_matches("/files", "/filestore/a.zip"));
        assert!(path_matches("/", "/anything"));
    }

    /// The expiry comparison is `<= now`, so a cookie expiring this very
    /// second is already gone. Session cookies (`0`) never expire.
    #[test]
    fn expiry_boundary_and_session_cookies() {
        let jar = [
            Cookie {
                domain: ".example.com".into(),
                include_subdomains: true,
                path: "/".into(),
                secure: false,
                expires: 1000,
                name: "exact".into(),
                value: "v".into(),
            },
            Cookie {
                domain: ".example.com".into(),
                include_subdomains: true,
                path: "/".into(),
                secure: false,
                expires: 0,
                name: "session".into(),
                value: "v".into(),
            },
        ];

        assert!(jar[0].matches("example.com", "/", false, 999), "one second early: still valid");
        assert!(!jar[0].matches("example.com", "/", false, 1000), "expiring now counts as expired");
        assert!(!jar[0].matches("example.com", "/", false, 1001));

        // A session cookie has no expiry at all, even far in the future.
        assert!(jar[1].matches("example.com", "/", false, u64::MAX));
    }

    #[test]
    fn httponly_prefixed_lines_are_cookies_not_comments() {
        let jar = parse(SAMPLE);
        assert!(
            jar.iter().any(|c| c.name == "session" && c.value == "keep"),
            "the #HttpOnly_ prefix must be stripped, not treated as a comment"
        );
        // Everything else starting with # is still a comment.
        assert!(!jar.iter().any(|c| c.name.starts_with('#')));
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let jar = parse(
            "\
too\tfew\tfields
.a.com\tTRUE\t/\tFALSE\tnot-a-number\tname\tvalue
.b.com\tTRUE\t/\tFALSE\t2000000000\t\tvalue
.good.com\tTRUE\t/\tFALSE\t2000000000\tkeeper\tyes
",
        );
        assert_eq!(jar.len(), 1, "only the well-formed line survives");
        assert_eq!(jar[0].name, "keeper");
    }

    #[test]
    fn negative_expiry_clamps_to_zero_not_a_huge_number() {
        // `expires as u64` on a negative float wraps to an enormous value,
        // which would make an expired cookie immortal.
        let jar = parse(".a.com\tTRUE\t/\tFALSE\t-5\tstale\tv\n");
        assert_eq!(jar[0].expires, 0);
    }

    #[test]
    fn host_matching_ignores_case() {
        let jar = parse(".Example.COM\tTRUE\t/\tFALSE\t2000000000\tk\tv\n");
        let url = Url::parse("http://WWW.example.com/a").unwrap();
        assert_eq!(header_for(&jar, &url).as_deref(), Some("k=v"));
    }

    #[test]
    fn header_joins_every_match_in_file_order() {
        let jar = parse(
            "\
.example.com\tTRUE\t/\tFALSE\t2000000000\tone\t1
.example.com\tTRUE\t/\tFALSE\t2000000000\ttwo\t2
",
        );
        let url = Url::parse("http://example.com/x").unwrap();
        assert_eq!(header_for(&jar, &url).as_deref(), Some("one=1; two=2"));
    }

    #[test]
    fn subdomain_rules() {
        assert!(domain_matches(".example.com", true, "example.com"));
        assert!(domain_matches(".example.com", true, "cdn.example.com"));
        assert!(domain_matches("example.com", false, "example.com"));
        assert!(!domain_matches("example.com", false, "cdn.example.com"));
        // Must not match a domain that merely ends with the same text.
        assert!(!domain_matches(".example.com", true, "notexample.com"));
    }
}
