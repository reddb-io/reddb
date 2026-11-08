import '../conn.dart';
import '../errors.dart';
import '../url.dart';

Future<Conn> openEmbedded(ParsedUri uri) {
  throw EmbeddedUnsupported(
    'embedded RedDB is unavailable on this Dart platform',
  );
}
