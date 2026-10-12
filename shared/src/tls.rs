//! The out-of-band SHA-256 pin is the trust anchor. TLS still verifies every
//! handshake signature; a matching certificate alone cannot impersonate its key.
use anyhow::{Context, Result};
use rustls::{client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier},
    pki_types::{CertificateDer,PrivateKeyDer,ServerName,UnixTime},
    DigitallySignedStruct,SignatureScheme};
use sha2::{Digest,Sha256};
use std::{fs,sync::Arc};
use subtle::ConstantTimeEq;

#[derive(Debug)]
struct PinnedVerifier { pin:[u8;32], provider:rustls::crypto::CryptoProvider }
impl ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(&self,end_entity:&CertificateDer<'_>,_intermediates:&[CertificateDer<'_>],_server_name:&ServerName<'_>,_ocsp:&[u8],_now:UnixTime)->Result<ServerCertVerified,rustls::Error> {
        let actual:[u8;32]=Sha256::digest(end_entity.as_ref()).into();
        if bool::from(actual.ct_eq(&self.pin)) {Ok(ServerCertVerified::assertion())} else {Err(rustls::Error::General("certificate pin mismatch".into()))}
    }
    fn verify_tls12_signature(&self,message:&[u8],cert:&CertificateDer<'_>,dss:&DigitallySignedStruct)->Result<HandshakeSignatureValid,rustls::Error> {
        rustls::crypto::verify_tls12_signature(message,cert,dss,&self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self,message:&[u8],cert:&CertificateDer<'_>,dss:&DigitallySignedStruct)->Result<HandshakeSignatureValid,rustls::Error> {
        rustls::crypto::verify_tls13_signature(message,cert,dss,&self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self)->Vec<SignatureScheme> {self.provider.signature_verification_algorithms.supported_schemes()}
}
pub fn client(pin:[u8;32])->Arc<rustls::ClientConfig> {
    Arc::new(rustls::ClientConfig::builder().dangerous().with_custom_certificate_verifier(Arc::new(PinnedVerifier{pin,provider:rustls::crypto::ring::default_provider()})).with_no_client_auth())
}
pub fn server_der(cert:Vec<u8>,key:Vec<u8>)->Result<Arc<rustls::ServerConfig>> {
    let key=PrivateKeyDer::try_from(key).map_err(|_|anyhow::anyhow!("invalid private key"))?;
    Ok(Arc::new(rustls::ServerConfig::builder().with_no_client_auth().with_single_cert(vec![CertificateDer::from(cert)],key)?))
}
pub fn server(cert:&str,key:&str)->Result<Arc<rustls::ServerConfig>> {server_der(fs::read(cert).context("could not read certificate")?,fs::read(key).context("could not read private key")?)}
