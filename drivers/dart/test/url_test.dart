import 'package:reddb/reddb.dart';
import 'package:test/test.dart';

void main() {
  group('parseUri', () {
    test('red:// default port 5050', () {
      final p = parseUri('red://localhost');
      expect(p.kind, 'redwire');
      expect(p.host, 'localhost');
      expect(p.port, 5050);
    });

    test('red:// explicit port', () {
      final p = parseUri('red://10.0.0.5:7777');
      expect(p.kind, 'redwire');
      expect(p.host, '10.0.0.5');
      expect(p.port, 7777);
    });

    test('reds:// default port 5050', () {
      final p = parseUri('reds://example.com');
      expect(p.kind, 'redwire-tls');
      expect(p.port, 5050);
    });

    test('http:// default port 8080', () {
      final p = parseUri('http://api.example.com');
      expect(p.kind, 'http');
      expect(p.port, 8080);
    });

    test('https:// default port 8443', () {
      final p = parseUri('https://api.example.com');
      expect(p.kind, 'https');
      expect(p.port, 8443);
    });

    test('user:pass percent-decoded', () {
      final p = parseUri('red://alice:p%40ssw0rd@host:5050');
      expect(p.username, 'alice');
      expect(p.password, 'p@ssw0rd');
    });

    test('?token= captured', () {
      final p = parseUri('red://host?token=sk-abc-123');
      expect(p.token, 'sk-abc-123');
    });

    test('?auth=scram captured', () {
      final p = parseUri('red://alice:secret@host?auth=scram');
      expect(p.auth, 'scram');
    });

    test('?auth= invalid raises', () {
      expect(() => parseUri('red://host?auth=garbage'), throwsA(isA<InvalidUri>()));
    });

    test('sslmode=require promotes to TLS', () {
      final p = parseUri('red://host?sslmode=require');
      expect(p.kind, 'redwire-tls');
    });

    test('?timeout_ms= integer', () {
      final p = parseUri('red://host?timeout_ms=12500');
      expect(p.timeoutMs, 12500);
    });

    test('?timeout_ms= invalid raises', () {
      expect(() => parseUri('red://host?timeout_ms=fast'), throwsA(isA<InvalidUri>()));
    });

    test('TLS files in query', () {
      final p = parseUri('reds://host?ca=/etc/ca.pem&cert=/etc/c.pem&key=/etc/k.pem');
      expect(p.ca, '/etc/ca.pem');
      expect(p.cert, '/etc/c.pem');
      expect(p.key, '/etc/k.pem');
    });

    test('proto override → https', () {
      final p = parseUri('red://host:9000?proto=https');
      expect(p.kind, 'https');
      expect(p.port, 9000);
    });

    test('proto override → reds', () {
      final p = parseUri('red://host?proto=reds');
      expect(p.kind, 'redwire-tls');
    });

    test('proto=grpc aliases redwire', () {
      final p = parseUri('red://host?proto=grpc');
      expect(p.kind, 'redwire');
    });

    test('embedded shortcuts', () {
      const cases = [
        'red://',
        'red:',
        'red://memory',
        'red://memory/',
        'red://:memory',
        'red://:memory:',
      ];
      for (final uri in cases) {
        final p = parseUri(uri);
        expect(p.kind, 'embedded', reason: uri);
        expect(p.path, isNull, reason: uri);
      }
    });

    test('embedded with absolute path', () {
      final p = parseUri('red:///var/lib/reddb/data.rdb');
      expect(p.kind, 'embedded');
      expect(p.path, '/var/lib/reddb/data.rdb');
    });

    test('unsupported scheme', () {
      expect(() => parseUri('mongodb://host'), throwsA(isA<UnsupportedScheme>()));
    });

    test('empty raises', () {
      expect(() => parseUri(''), throwsA(isA<InvalidUri>()));
    });

    test('defaultPortFor aliases', () {
      expect(defaultPortFor('redwire'), 5050);
      expect(defaultPortFor('http'), 8080);
      expect(defaultPortFor('https'), 8443);
      expect(defaultPortFor('nonsense'), 5050);
    });

    test('?token + user:pass kept together', () {
      final p = parseUri('red://u:pw@host?token=tok');
      expect(p.username, 'u');
      expect(p.password, 'pw');
      expect(p.token, 'tok');
    });

    test('originalUri preserved', () {
      const uri = 'red://host?proto=https&token=x';
      final p = parseUri(uri);
      expect(p.originalUri, uri);
    });

    test('proto unknown raises', () {
      expect(() => parseUri('red://host?proto=ftp'), throwsA(isA<UnsupportedScheme>()));
    });

    test('deriveLoginUrl uses HTTP base', () {
      final p = parseUri('http://host:1234');
      expect(deriveLoginUrl(p), 'http://host:1234/auth/login');
    });

    test('deriveLoginUrl falls back to HTTPS for redwire', () {
      final p = parseUri('red://api.example.com');
      expect(deriveLoginUrl(p), 'https://api.example.com/auth/login');
    });
  });
}
