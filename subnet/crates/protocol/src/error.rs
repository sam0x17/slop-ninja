use core::fmt;

pub type Result<T> = core::result::Result<T, Error>;

/// Every rejection has a stable code so fixtures compare identically across targets.
/// `Deferred` is transient: the refused operation waits for later finalized
/// state, and nothing else pauses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    UnexpectedEnd,
    TrailingBytes,
    InvalidBool(u8),
    InvalidTag { field: &'static str, tag: u8 },
    InvalidUtf8,
    LengthLimit { field: &'static str, len: u64 },
    WrongKind { expected: &'static str },
    WrongVersion { kind: &'static str, version: u16 },
    NonCanonical,
    Invalid(&'static str),
    OutOfRange(&'static str),
    Unsorted(&'static str),
    Duplicate(&'static str),
    Mismatch(&'static str),
    Overflow(&'static str),
    Expired(&'static str),
    Deferred(&'static str),
    WrongRole,
    UntrustedKey,
    DuplicateTrustedKey,
    BadPublicKey,
    BadSignature,
}

impl Error {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnexpectedEnd => "unexpected_end",
            Self::TrailingBytes => "trailing_bytes",
            Self::InvalidBool(_) => "invalid_bool",
            Self::InvalidTag { .. } => "invalid_tag",
            Self::InvalidUtf8 => "invalid_utf8",
            Self::LengthLimit { .. } => "length_limit",
            Self::WrongKind { .. } => "wrong_kind",
            Self::WrongVersion { .. } => "wrong_version",
            Self::NonCanonical => "non_canonical",
            Self::Invalid(_) => "invalid",
            Self::OutOfRange(_) => "out_of_range",
            Self::Unsorted(_) => "unsorted",
            Self::Duplicate(_) => "duplicate",
            Self::Mismatch(_) => "mismatch",
            Self::Overflow(_) => "overflow",
            Self::Expired(_) => "expired",
            Self::Deferred(_) => "deferred",
            Self::WrongRole => "wrong_role",
            Self::UntrustedKey => "untrusted_key",
            Self::DuplicateTrustedKey => "duplicate_trusted_key",
            Self::BadPublicKey => "bad_public_key",
            Self::BadSignature => "bad_signature",
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBool(b) => write!(f, "invalid_bool: {b}"),
            Self::InvalidTag { field, tag } => write!(f, "invalid_tag: {field} {tag}"),
            Self::LengthLimit { field, len } => write!(f, "length_limit: {field} {len}"),
            Self::WrongKind { expected } => write!(f, "wrong_kind: expected {expected}"),
            Self::WrongVersion { kind, version } => write!(f, "wrong_version: {kind} v{version}"),
            Self::Invalid(what)
            | Self::OutOfRange(what)
            | Self::Unsorted(what)
            | Self::Duplicate(what)
            | Self::Mismatch(what)
            | Self::Overflow(what)
            | Self::Expired(what)
            | Self::Deferred(what) => write!(f, "{}: {what}", self.code()),
            _ => f.write_str(self.code()),
        }
    }
}

impl std::error::Error for Error {}
