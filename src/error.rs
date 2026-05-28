use thiserror::Error;

#[derive(Debug, Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum PqcError {
    #[error("network error")]
    Network,
    #[error("TLS error")]
    Tls,
    #[error("request timed out")]
    Timeout,
    #[error("invalid request")]
    InvalidRequest,
    #[error("invalid response")]
    InvalidResponse,
    #[error("certificate pinning failure")]
    PinningFailure,
    #[error("trust verification failure")]
    TrustVerification,
}
