// Driver-wide error set. The set is intentionally narrow — most
// failure modes come back from std lib calls and we surface those
// as-is (e.g. `error.ConnectionRefused`). The custom errors here
// are reserved for protocol-level violations the std lib can't
// describe.

pub const Error = error{
    // URI parser
    UnparseableUri,
    UnsupportedScheme,
    UnsupportedProto,
    MissingHost,
    InvalidPort,

    // Frame codec
    FrameTruncated,
    FrameInvalidLength,
    FrameUnknownKind,
    FrameUnknownFlags,
    FrameTooLarge,
    CompressedButNoZstd,
    DecompressFailed,

    // Handshake / protocol
    ProtocolError,
    AuthRefused,
    BadMagic,
    UnsupportedVersion,
    UnexpectedFrame,

    // SCRAM
    ScramBadServerFirst,
    ScramServerSignatureMismatch,

    // HTTP
    HttpStatus,
    HttpBadResponse,
};
