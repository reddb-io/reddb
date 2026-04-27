/// Pure-Dart driver for RedDB.
///
/// Speaks the RedWire binary protocol (TCP / TLS) and the REST/HTTP
/// transport. No FFI on the wire path — runs on Flutter mobile, desktop,
/// and (HTTP only) on the web.
library;

export 'src/conn.dart' show Conn;
export 'src/errors.dart';
export 'src/options.dart';
export 'src/reddb_base.dart' show Reddb, connect;
export 'src/url.dart' show ParsedUri, parseUri, defaultPortFor, deriveLoginUrl;
export 'src/redwire/codec.dart' show ZstdCodec;
export 'src/redwire/frame.dart'
    show
        encodeFrame,
        decodeFrame,
        Frame,
        FrameHeader,
        MessageKind,
        Flags,
        FRAME_HEADER_SIZE,
        MAX_FRAME_SIZE,
        KNOWN_FLAGS,
        MAGIC,
        SUPPORTED_VERSION;
