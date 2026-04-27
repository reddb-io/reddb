package redwire

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"fmt"
	"strconv"
	"strings"
)

// SCRAM-SHA-256 client primitives. RFC 5802 + RFC 7677.
//
// Hand-rolled HMAC / PBKDF2 to keep the dependency tree minimal, even though
// crypto/hmac and golang.org/x/crypto/pbkdf2 exist — staying inline mirrors
// drivers/rust/src/redwire/scram.rs and lets us test against RFC vectors
// without an external crate.

// DefaultIter mirrors the engine's RFC 7677 default.
const DefaultIter = 16384

// MinIter is the lowest iteration count this client will agree to use.
const MinIter = 4096

// HMACSHA256 returns HMAC-SHA-256(key, data). Implemented inline to match the
// Rust client byte-for-byte.
func HMACSHA256(key, data []byte) [32]byte {
	mac := hmac.New(sha256.New, key)
	mac.Write(data)
	var out [32]byte
	copy(out[:], mac.Sum(nil))
	return out
}

// SHA256 returns SHA-256(data).
func SHA256(data []byte) [32]byte {
	return sha256.Sum256(data)
}

// PBKDF2SHA256 derives a 32-byte key. Iter must be ≥ 1; callers should
// pass MinIter or higher.
func PBKDF2SHA256(password, salt []byte, iter uint32) [32]byte {
	// Block index always 1 because we want exactly one 32-byte block.
	salted := make([]byte, 0, len(salt)+4)
	salted = append(salted, salt...)
	idx := make([]byte, 4)
	binary.BigEndian.PutUint32(idx, 1)
	salted = append(salted, idx...)
	u := HMACSHA256(password, salted)
	out := u
	for i := uint32(1); i < iter; i++ {
		u = HMACSHA256(password, u[:])
		for j := 0; j < 32; j++ {
			out[j] ^= u[j]
		}
	}
	return out
}

// XOR returns a^b. Inputs must be the same length.
func XOR(a, b []byte) []byte {
	if len(a) != len(b) {
		return nil
	}
	out := make([]byte, len(a))
	for i := range a {
		out[i] = a[i] ^ b[i]
	}
	return out
}

// AuthMessage builds the canonical RFC 5802 §3 auth-message string:
//
//	client-first-bare + "," + server-first + "," + client-final-no-proof
func AuthMessage(clientFirstBare, serverFirst, clientFinalNoProof string) []byte {
	return []byte(clientFirstBare + "," + serverFirst + "," + clientFinalNoProof)
}

// ClientProof — what the client sends to prove it knows the password.
//
//	salted    = PBKDF2-HMAC-SHA256(password, salt, iter, 32)
//	clientKey = HMAC-SHA256(salted, "Client Key")
//	storedKey = SHA256(clientKey)
//	signature = HMAC-SHA256(storedKey, authMessage)
//	proof     = clientKey XOR signature
func ClientProof(password, salt []byte, iter uint32, authMessage []byte) []byte {
	salted := PBKDF2SHA256(password, salt, iter)
	clientKey := HMACSHA256(salted[:], []byte("Client Key"))
	storedKey := SHA256(clientKey[:])
	sig := HMACSHA256(storedKey[:], authMessage)
	return XOR(clientKey[:], sig[:])
}

// VerifyServerSignature checks the `v=` field in AuthOk against the recomputed
// HMAC-SHA-256(serverKey, authMessage). Constant-time comparison.
func VerifyServerSignature(password, salt []byte, iter uint32, authMessage, presented []byte) bool {
	if len(presented) != 32 {
		return false
	}
	salted := PBKDF2SHA256(password, salt, iter)
	serverKey := HMACSHA256(salted[:], []byte("Server Key"))
	expected := HMACSHA256(serverKey[:], authMessage)
	return subtle.ConstantTimeCompare(expected[:], presented) == 1
}

// NewClientNonce returns a fresh 24-byte client nonce, base64-std encoded
// without padding stripped (matches what the Rust driver and the engine emit).
func NewClientNonce() (string, error) {
	var raw [18]byte
	if _, err := rand.Read(raw[:]); err != nil {
		return "", fmt.Errorf("redwire scram: random: %w", err)
	}
	return base64.StdEncoding.EncodeToString(raw[:]), nil
}

// ScramServerFirst is the parsed `r=...,s=...,i=...` payload the server sends
// in its AuthRequest after we send client-first.
type ScramServerFirst struct {
	CombinedNonce string
	Salt          []byte
	Iter          uint32
	Raw           string // original textual form, used to build authMessage
}

// ParseServerFirst parses the server-first message bytes.
func ParseServerFirst(raw []byte) (*ScramServerFirst, error) {
	s := string(raw)
	out := &ScramServerFirst{Raw: s}
	for _, part := range strings.Split(s, ",") {
		switch {
		case strings.HasPrefix(part, "r="):
			out.CombinedNonce = part[2:]
		case strings.HasPrefix(part, "s="):
			salt, err := base64.StdEncoding.DecodeString(part[2:])
			if err != nil {
				return nil, fmt.Errorf("redwire scram: bad salt: %w", err)
			}
			out.Salt = salt
		case strings.HasPrefix(part, "i="):
			n, err := strconv.ParseUint(part[2:], 10, 32)
			if err != nil {
				return nil, fmt.Errorf("redwire scram: bad iter: %w", err)
			}
			out.Iter = uint32(n)
		}
	}
	if out.CombinedNonce == "" {
		return nil, errors.New("redwire scram: server-first missing r=")
	}
	if len(out.Salt) == 0 {
		return nil, errors.New("redwire scram: server-first missing s=")
	}
	if out.Iter == 0 {
		return nil, errors.New("redwire scram: server-first missing i=")
	}
	return out, nil
}

// EncodeBase64Std returns base64-std encoding of input (with `=` padding).
func EncodeBase64Std(input []byte) string {
	return base64.StdEncoding.EncodeToString(input)
}

// DecodeBase64Std accepts both padded and unpadded forms.
func DecodeBase64Std(input string) ([]byte, error) {
	if strings.HasSuffix(input, "=") {
		return base64.StdEncoding.DecodeString(input)
	}
	return base64.RawStdEncoding.DecodeString(input)
}

// ScramSession packages the client-side state needed to drive a SCRAM exchange
// across the three RTTs in the RedWire handshake.
type ScramSession struct {
	Username        string
	Password        []byte
	ClientNonce     string
	ClientFirstBare string
}

// NewScramSession seeds a session with a fresh nonce.
func NewScramSession(username, password string) (*ScramSession, error) {
	nonce, err := NewClientNonce()
	if err != nil {
		return nil, err
	}
	return &ScramSession{
		Username:        username,
		Password:        []byte(password),
		ClientNonce:     nonce,
		ClientFirstBare: "n=" + username + ",r=" + nonce,
	}, nil
}

// ClientFirstMessage returns the GS2 + bare form to send in AuthResponse.
func (s *ScramSession) ClientFirstMessage() string {
	return "n,," + s.ClientFirstBare
}

// BuildClientFinal computes the client-final message + auth-message bytes
// based on the parsed server-first.
func (s *ScramSession) BuildClientFinal(sf *ScramServerFirst) (final string, authMessage []byte, err error) {
	if !strings.HasPrefix(sf.CombinedNonce, s.ClientNonce) {
		return "", nil, errors.New("redwire scram: server combined nonce does not include our client nonce")
	}
	if sf.Iter < MinIter {
		return "", nil, fmt.Errorf("redwire scram: iter %d below minimum %d", sf.Iter, MinIter)
	}
	clientFinalNoProof := "c=biws,r=" + sf.CombinedNonce
	authMessage = AuthMessage(s.ClientFirstBare, sf.Raw, clientFinalNoProof)
	proof := ClientProof(s.Password, sf.Salt, sf.Iter, authMessage)
	final = clientFinalNoProof + ",p=" + EncodeBase64Std(proof)
	return final, authMessage, nil
}
