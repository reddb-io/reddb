/// SCRAM-SHA-256 client primitives (RFC 5802 / RFC 7677).
///
/// Pure functions, no I/O. Mirrors `drivers/rust/src/redwire/scram.rs`
/// and the Python driver so all three are bit-for-bit interoperable
/// with the engine's `src/auth/scram.rs`.
library;

import 'dart:convert';
import 'dart:math' show Random;
import 'dart:typed_data';

import 'package:crypto/crypto.dart';

const int defaultIter = 16384;
const int minIter = 4096;

Uint8List hmacSha256(List<int> key, List<int> data) {
  final mac = Hmac(sha256, key).convert(data);
  return Uint8List.fromList(mac.bytes);
}

Uint8List sha256Of(List<int> data) {
  return Uint8List.fromList(sha256.convert(data).bytes);
}

/// PBKDF2-HMAC-SHA256 with a fixed 32-byte derived key.
///
/// Hand-rolled because we don't want to drag in `package:cryptography`
/// just for this. Loop count `iterations` matches RFC 8018 / 6070.
Uint8List pbkdf2Sha256(
  List<int> password,
  List<int> salt,
  int iterations, {
  int dkLen = 32,
}) {
  if (iterations < 1) {
    throw ArgumentError.value(iterations, 'iterations', 'must be >= 1');
  }
  // For dkLen <= 32 we only need block index 1.
  final blockCount = (dkLen + 31) ~/ 32;
  final out = BytesBuilder(copy: false);
  for (var i = 1; i <= blockCount; i++) {
    final saltedBlock = Uint8List(salt.length + 4);
    saltedBlock.setRange(0, salt.length, salt);
    saltedBlock[salt.length] = (i >> 24) & 0xFF;
    saltedBlock[salt.length + 1] = (i >> 16) & 0xFF;
    saltedBlock[salt.length + 2] = (i >> 8) & 0xFF;
    saltedBlock[salt.length + 3] = i & 0xFF;
    var u = hmacSha256(password, saltedBlock);
    final block = Uint8List.fromList(u);
    for (var iter = 1; iter < iterations; iter++) {
      u = hmacSha256(password, u);
      for (var j = 0; j < 32; j++) {
        block[j] ^= u[j];
      }
    }
    out.add(block);
  }
  final all = out.toBytes();
  return all.length == dkLen ? all : Uint8List.sublistView(all, 0, dkLen);
}

Uint8List xorBytes(List<int> a, List<int> b) {
  if (a.length != b.length) {
    throw ArgumentError('xor inputs must be equal length');
  }
  final out = Uint8List(a.length);
  for (var i = 0; i < a.length; i++) {
    out[i] = a[i] ^ b[i];
  }
  return out;
}

/// Standard-alphabet base64 with padding — matches the server's
/// `base64_std`.
String b64encode(List<int> data) => base64.encode(data);

/// Standard-alphabet base64 decode. Tolerant of missing padding.
Uint8List b64decode(String text) {
  final pad = (-text.length) % 4;
  return Uint8List.fromList(base64.decode(text + ('=' * pad)));
}

/// 24-character base64 nonce produced from 18 random bytes.
///
/// Uses [Random.secure] when available; falls back to [Random] only if
/// the platform refuses (very unusual).
String makeClientNonce({int numBytes = 18, Random? random}) {
  final rng = random ?? Random.secure();
  final raw = Uint8List(numBytes);
  for (var i = 0; i < numBytes; i++) {
    raw[i] = rng.nextInt(256);
  }
  return b64encode(raw);
}

/// Build the client-first message and its bare form.
///
/// Returns `(client_first_message, client_first_bare)`. The bare form
/// is needed for the auth message later.
({String full, String bare}) buildClientFirst(String username, String clientNonce) {
  final bare = 'n=$username,r=$clientNonce';
  return (full: 'n,,$bare', bare: bare);
}

/// Parsed server-first message.
class ServerFirst {
  ServerFirst({
    required this.combinedNonce,
    required this.salt,
    required this.iterations,
  });

  final String combinedNonce;
  final Uint8List salt;
  final int iterations;
}

ServerFirst parseServerFirst(String serverFirst, String clientNonce) {
  String? combined;
  String? saltB64;
  int? iters;
  for (final part in serverFirst.split(',')) {
    if (part.startsWith('r=')) {
      combined = part.substring(2);
    } else if (part.startsWith('s=')) {
      saltB64 = part.substring(2);
    } else if (part.startsWith('i=')) {
      final n = int.tryParse(part.substring(2));
      if (n == null) {
        throw FormatException("invalid iter in server-first: '$part'");
      }
      iters = n;
    }
  }
  if (combined == null || saltB64 == null || iters == null) {
    throw FormatException("server-first missing fields: '$serverFirst'");
  }
  if (!combined.startsWith(clientNonce)) {
    throw FormatException(
      'server nonce does not start with client nonce (replay protection)',
    );
  }
  if (iters < minIter) {
    throw FormatException(
      'server-supplied iter ($iters) is below MIN_ITER=$minIter',
    );
  }
  return ServerFirst(
    combinedNonce: combined,
    salt: b64decode(saltB64),
    iterations: iters,
  );
}

/// Concatenate `client_first_bare,server_first,client_final_no_proof`.
Uint8List authMessage(
  String clientFirstBare,
  String serverFirst,
  String clientFinalNoProof,
) {
  return Uint8List.fromList(
    utf8.encode('$clientFirstBare,$serverFirst,$clientFinalNoProof'),
  );
}

/// Compute `ClientKey XOR HMAC(StoredKey, AuthMessage)` (32 bytes).
Uint8List clientProof(
  List<int> password,
  List<int> salt,
  int iterations,
  List<int> am,
) {
  final salted = pbkdf2Sha256(password, salt, iterations);
  final clientKey = hmacSha256(salted, utf8.encode('Client Key'));
  final storedKey = sha256Of(clientKey);
  final sig = hmacSha256(storedKey, am);
  return xorBytes(clientKey, sig);
}

/// Build the client-final message and the no-proof prefix.
({String full, String noProof}) buildClientFinal(
  String combinedNonce,
  Uint8List proof,
) {
  final noProof = 'c=biws,r=$combinedNonce';
  return (full: '$noProof,p=${b64encode(proof)}', noProof: noProof);
}

Uint8List serverSignature(
  List<int> password,
  List<int> salt,
  int iterations,
  List<int> am,
) {
  final salted = pbkdf2Sha256(password, salt, iterations);
  final serverKey = hmacSha256(salted, utf8.encode('Server Key'));
  return hmacSha256(serverKey, am);
}

bool constantTimeEq(List<int> a, List<int> b) {
  if (a.length != b.length) return false;
  var diff = 0;
  for (var i = 0; i < a.length; i++) {
    diff |= a[i] ^ b[i];
  }
  return diff == 0;
}

bool verifyServerSignature(
  List<int> password,
  List<int> salt,
  int iterations,
  List<int> am,
  List<int> presented,
) {
  if (presented.length != 32) return false;
  final expected = serverSignature(password, salt, iterations, am);
  return constantTimeEq(expected, presented);
}
