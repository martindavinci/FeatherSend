//! Tells whether GitHub has a newer release than this copy (check + notify
//! only, never auto-install).

use std::sync::Arc;
use std::time::Duration;

pub const REPO: &str = "martindavinci/FeatherSend";

/// A release number, `major.minor.patch`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// Accepts `v0.5.0` and `0.5.0`.
    pub fn parse(s: &str) -> Option<Version> {
        let mut parts = s.strip_prefix('v').unwrap_or(s).split('.');
        let mut next = || parts.next()?.parse::<u32>().ok();
        let version = Version(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }

    pub fn current() -> Version {
        Version::parse(env!("CARGO_PKG_VERSION")).expect("the package version is x.y.z")
    }
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: Version,
    pub url: String,
}

/// Reads the GitHub "latest release" JSON. Drafts and pre-releases are ignored.
pub fn parse_latest(json: &str) -> Option<Release> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    if value["draft"].as_bool() == Some(true) || value["prerelease"].as_bool() == Some(true) {
        return None;
    }
    Some(Release {
        version: Version::parse(value["tag_name"].as_str()?)?,
        url: value["html_url"].as_str()?.to_string(),
    })
}

/// The latest release if it is newer than this build. Uses the bundled
/// Mozilla roots, so an outdated Windows 7 root store does not matter.
pub async fn check_latest(repo: &str) -> anyhow::Result<Option<Release>> {
    use localsend::reqwest;
    let _ = rustls::crypto::ring::default_provider().install_default();
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let client = reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .timeout(Duration::from_secs(10))
        .user_agent(format!("FeatherSend/{}", Version::current()))
        .build()?;
    let body = client
        .get(format!("https://api.github.com/repos/{repo}/releases/latest"))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(parse_latest(&body).filter(|r| r.version > Version::current()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_versions() {
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.10.0"), Some(Version(0, 10, 0)));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("1.2.3.4"), None);
        assert_eq!(Version::parse("1.2.x"), None);
    }

    #[test]
    fn orders_versions() {
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
    }

    #[test]
    fn parse_latest_reads_tag_and_url() {
        let json = r#"{"tag_name":"v0.2.0","html_url":"https://github.com/x/y/releases/tag/v0.2.0","draft":false,"prerelease":false}"#;
        assert_eq!(
            parse_latest(json),
            Some(Release {
                version: Version(0, 2, 0),
                url: "https://github.com/x/y/releases/tag/v0.2.0".into()
            })
        );
    }

    #[test]
    fn drafts_ignored() {
        let json = r#"{"tag_name":"v9.0.0","html_url":"u","draft":true}"#;
        assert_eq!(parse_latest(json), None);
        assert_eq!(parse_latest("not json"), None);
    }
}
