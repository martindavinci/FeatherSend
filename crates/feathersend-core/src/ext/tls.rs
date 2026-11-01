//! Mutual TLS for the extension protocol. Both sides present their identity
//! certificate; the server accepts any valid self-signed client certificate
//! and hands its fingerprint to the handlers, the client pins the server's
//! fingerprint during the handshake (so nothing is sent to an impostor).
//! Adapted from LocalSend core's verifiers, which are crate-private there.

use crate::identity::Identity;
use localsend::crypto::cert::{fingerprint_from_cert_der, verify_cert_from_der};
use rustls::client::WebPkiServerVerifier;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::server::WebPkiClientVerifier;
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, Error, RootCertStore, SignatureScheme};
use std::sync::Arc;

pub fn ensure_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}

fn own_root_store(cert_pem: &str) -> anyhow::Result<Arc<RootCertStore>> {
    // Only so the WebPKI builders get a non-empty store; never used as an
    // authority, the checks below do not delegate chain building.
    let mut roots = RootCertStore::empty();
    roots.add(CertificateDer::from_pem_slice(cert_pem.as_bytes())?)?;
    Ok(Arc::new(roots))
}

fn cert_chain_and_key(
    id: &Identity,
) -> anyhow::Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    Ok((
        vec![CertificateDer::from_pem_slice(id.cert_pem.as_bytes())?],
        PrivateKeyDer::from_pem_slice(id.key_pem.as_bytes())?,
    ))
}

fn invalid(e: impl std::fmt::Display) -> Error {
    tracing::warn!("peer certificate rejected: {e}");
    Error::InvalidCertificate(rustls::CertificateError::ApplicationVerificationFailure)
}

#[derive(Debug)]
struct AnyValidClientCert {
    inner: Arc<dyn ClientCertVerifier>,
}

impl ClientCertVerifier for AnyValidClientCert {
    fn offer_client_auth(&self) -> bool {
        true
    }

    fn client_auth_mandatory(&self) -> bool {
        true
    }

    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        self.inner.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        cert: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        verify_cert_from_der(cert.as_ref(), None).map_err(invalid)?;
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

#[derive(Debug)]
struct PinnedServerCert {
    inner: Arc<dyn ServerCertVerifier>,
    expected: String,
}

impl ServerCertVerifier for PinnedServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _: &[CertificateDer<'_>],
        _: &ServerName<'_>,
        _: &[u8],
        _: UnixTime,
    ) -> Result<ServerCertVerified, Error> {
        // Peers are addressed by IP: the fingerprint identifies them, not a name.
        verify_cert_from_der(end_entity.as_ref(), None).map_err(invalid)?;
        let actual = fingerprint_from_cert_der(end_entity.as_ref());
        if !actual.eq_ignore_ascii_case(&self.expected) {
            return Err(invalid(format!("fingerprint {actual}, expected {}", self.expected)));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        self.inner.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.inner.supported_verify_schemes()
    }
}

pub fn server_config(id: &Identity) -> anyhow::Result<Arc<rustls::ServerConfig>> {
    ensure_provider();
    let verifier = AnyValidClientCert {
        inner: WebPkiClientVerifier::builder(own_root_store(&id.cert_pem)?).build()?,
    };
    let (chain, key) = cert_chain_and_key(id)?;
    let mut config = rustls::ServerConfig::builder()
        .with_client_cert_verifier(Arc::new(verifier))
        .with_single_cert(chain, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// Client config presenting `id` and accepting only a server whose
/// certificate fingerprint is `expected_fp`.
pub fn client_config(id: &Identity, expected_fp: &str) -> anyhow::Result<rustls::ClientConfig> {
    ensure_provider();
    let verifier = PinnedServerCert {
        inner: WebPkiServerVerifier::builder(own_root_store(&id.cert_pem)?).build()?,
        expected: expected_fp.to_ascii_uppercase(),
    };
    let (chain, key) = cert_chain_and_key(id)?;
    let mut config = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_client_auth_cert(chain, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(config)
}

/// Fingerprint of the client certificate of an established connection.
pub fn peer_fingerprint(conn: &rustls::ServerConnection) -> Option<String> {
    conn.peer_certificates()?.first().map(|c| fingerprint_from_cert_der(c.as_ref()))
}
