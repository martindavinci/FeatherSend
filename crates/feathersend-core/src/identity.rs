//! The device identity: one self-signed certificate per installation. Its
//! fingerprint identifies this device to LocalSend and FeatherSend peers alike.

use anyhow::{Context, bail};
use localsend::crypto::cert::{fingerprint_from_cert_der, generate_self_signed};
use std::path::Path;

#[derive(Clone, Debug)]
pub struct Identity {
    pub cert_pem: String,
    pub key_pem: String,
    /// SHA-256 of the certificate DER, UPPERCASE hex (LocalSend's format).
    pub fingerprint: String,
}

impl Identity {
    /// Loads `path` (certificate PEM followed by key PEM), or generates and
    /// saves a new identity when the file does not exist. RSA key generation
    /// takes a while on old CPUs: call it from `spawn_blocking`.
    ///
    /// A file that exists but cannot be parsed is an error and is left as is:
    /// replacing it would silently break every existing pairing.
    pub fn load_or_generate(path: &Path) -> anyhow::Result<Identity> {
        match std::fs::read_to_string(path) {
            Ok(text) => Identity::parse(&text).with_context(|| format!("{}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let generated = generate_self_signed()?;
                let identity = Identity {
                    cert_pem: generated.certificate_pem,
                    key_pem: generated.private_key_pem,
                    fingerprint: generated.fingerprint.to_ascii_uppercase(),
                };
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                crate::settings::write_atomic(
                    path,
                    format!("{}{}", identity.cert_pem, identity.key_pem).as_bytes(),
                )?;
                Ok(identity)
            }
            Err(e) => Err(e.into()),
        }
    }

    fn parse(text: &str) -> anyhow::Result<Identity> {
        let blocks = pem::parse_many(text)?;
        let cert = blocks.iter().find(|b| b.tag() == "CERTIFICATE").context("no certificate")?;
        let key =
            blocks.iter().find(|b| b.tag().ends_with("PRIVATE KEY")).context("no private key")?;
        if key.contents().is_empty() {
            bail!("empty private key");
        }
        Ok(Identity {
            cert_pem: pem::encode(cert),
            key_pem: pem::encode(key),
            fingerprint: fingerprint_from_cert_der(cert.contents()).to_ascii_uppercase(),
        })
    }
}

pub fn fingerprints_equal(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::OnceLock;

    /// RSA generation is slow: tests share two identities.
    pub(crate) fn test_identity(n: usize) -> Identity {
        static IDS: OnceLock<Vec<Identity>> = OnceLock::new();
        IDS.get_or_init(|| {
            (0..3)
                .map(|i| {
                    let dir = std::env::temp_dir()
                        .join(format!("feathersend-test-id-{}-{i}", std::process::id()));
                    Identity::load_or_generate(&dir.join("identity.pem")).unwrap()
                })
                .collect()
        })[n]
            .clone()
    }

    #[test]
    fn generates_then_reloads_same_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity").join("identity.pem");
        let first = Identity::load_or_generate(&path).unwrap();
        let second = Identity::load_or_generate(&path).unwrap();
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.cert_pem, second.cert_pem);
    }

    #[test]
    fn fingerprint_is_uppercase_hex_64() {
        let id = test_identity(0);
        assert_eq!(id.fingerprint.len(), 64);
        assert!(id.fingerprint.chars().all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()));
    }

    #[test]
    fn corrupt_file_is_error_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity.pem");
        std::fs::write(&path, "garbage").unwrap();
        assert!(Identity::load_or_generate(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "garbage");
    }

    #[test]
    fn fingerprints_compare_case_insensitive() {
        assert!(fingerprints_equal("ABCDEF", "abcdef"));
        assert!(!fingerprints_equal("ABCDEF", "ABCDEE"));
    }
}
