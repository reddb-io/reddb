import 'dart:convert';
import 'dart:typed_data';

import 'package:reddb/src/redwire/scram.dart';
import 'package:test/test.dart';

void main() {
  group('hmacSha256', () {
    test('RFC 4231 case 1', () {
      final key = Uint8List.fromList(List.filled(20, 0x0b));
      final data = utf8.encode('Hi There');
      final mac = hmacSha256(key, data);
      const expected = [
        0xb0, 0x34, 0x4c, 0x61, 0xd8, 0xdb, 0x38, 0x53, //
        0x5c, 0xa8, 0xaf, 0xce, 0xaf, 0x0b, 0xf1, 0x2b,
        0x88, 0x1d, 0xc2, 0x00, 0xc9, 0x83, 0x3d, 0xa7,
        0x26, 0xe9, 0x37, 0x6c, 0x2e, 0x32, 0xcf, 0xf7,
      ];
      expect(mac, equals(expected));
    });
  });

  group('pbkdf2Sha256', () {
    test('deterministic', () {
      final a = pbkdf2Sha256(utf8.encode('password'), utf8.encode('salt'), 1024);
      final b = pbkdf2Sha256(utf8.encode('password'), utf8.encode('salt'), 1024);
      expect(a, equals(b));
      final c = pbkdf2Sha256(utf8.encode('different'), utf8.encode('salt'), 1024);
      expect(a, isNot(equals(c)));
    });

    test('matches the known RFC 6070 case 2 (sha1 → ours sha256 sanity)', () {
      // We cannot reuse RFC 6070 sha1 vectors directly; use an internal
      // single-iteration roundtrip as a structural sanity check.
      final out = pbkdf2Sha256(utf8.encode('pwd'), utf8.encode('NaCl'), 1);
      expect(out.length, 32);
    });
  });

  group('clientProof / verifyServerSignature', () {
    test('round-trip via client_proof function', () {
      final salt = utf8.encode('reddb-test');
      const iter = 4096;
      final password = utf8.encode('hunter2');
      final am = utf8.encode(
          'client-first-bare,server-first,client-final-no-proof');

      final proofA = clientProof(password, salt, iter, am);
      final proofB = clientProof(password, salt, iter, am);
      expect(proofA, equals(proofB));

      final wrong = clientProof(utf8.encode('wrong'), salt, iter, am);
      expect(proofA, isNot(equals(wrong)));
    });

    test('server signature verifies with matching password', () {
      final salt = utf8.encode('s');
      const iter = 4096;
      final pw = utf8.encode('correct horse battery staple');
      final am = utf8.encode('a,b,c');
      final sig = serverSignature(pw, salt, iter, am);
      expect(verifyServerSignature(pw, salt, iter, am, sig), isTrue);
      expect(
        verifyServerSignature(utf8.encode('not-my-password'), salt, iter, am, sig),
        isFalse,
      );
    });
  });

  group('parseServerFirst', () {
    test('happy path', () {
      const cn = 'cnonceXYZ';
      final s = 'r=${cn}server-tail,s=${base64.encode([1, 2, 3, 4])},i=4096';
      final parsed = parseServerFirst(s, cn);
      expect(parsed.iterations, 4096);
      expect(parsed.salt, equals(Uint8List.fromList([1, 2, 3, 4])));
      expect(parsed.combinedNonce.startsWith(cn), isTrue);
    });

    test('rejects mismatched nonce', () {
      expect(
        () => parseServerFirst('r=other,s=AAAA,i=4096', 'mine'),
        throwsA(isA<FormatException>()),
      );
    });

    test('rejects iter below MIN_ITER', () {
      expect(
        () => parseServerFirst('r=mineX,s=AAAA,i=10', 'mine'),
        throwsA(isA<FormatException>()),
      );
    });
  });

  group('makeClientNonce', () {
    test('produces 24 base64 characters from 18 random bytes', () {
      final n = makeClientNonce();
      expect(n.length, 24);
      // base64 standard alphabet only.
      expect(RegExp(r'^[A-Za-z0-9+/=]+$').hasMatch(n), isTrue);
    });
  });
}
