package redwire

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net"
	"sync"
	"sync/atomic"
	"time"
)

// Magic + minor-version bytes that prefix every RedWire connection.
const (
	Magic            byte = 0xFE
	SupportedVersion byte = 0x01
)

// AuthMethod selects the credentials a client offers in Hello.
type AuthMethod int

const (
	// AuthAnonymous — server must allow anonymous (auth disabled).
	AuthAnonymous AuthMethod = iota
	// AuthBearer — token (login session or API key).
	AuthBearer
	// AuthScram — username + password via SCRAM-SHA-256 (3 RTTs).
	AuthScram
	// AuthOAuthJWT — JWT bearer issued by an OAuth provider.
	AuthOAuthJWT
)

// AuthCreds carries the secret material for the chosen method. Only the field
// matching Method is consulted.
type AuthCreds struct {
	Method   AuthMethod
	Token    string // bearer
	Username string // scram
	Password string // scram
	JWT      string // oauth-jwt
}

// TLSConfig configures the optional TLS layer.
type TLSConfig struct {
	// ServerName overrides SNI. Defaults to the dial host.
	ServerName string
	// RootCAs is the trusted CA bundle. nil = use system roots.
	RootCAs *x509.CertPool
	// Certificates carries client certs for mTLS.
	Certificates []tls.Certificate
	// InsecureSkipVerify disables cert verification. Dev only.
	InsecureSkipVerify bool
}

// ConnOptions controls the dial + handshake.
type ConnOptions struct {
	Host        string
	Port        int
	Auth        AuthCreds
	ClientName  string
	TLS         *TLSConfig
	DialTimeout time.Duration
}

// Conn is a single, sequential RedWire connection. Reads and writes are
// serialised through a mutex; correlation IDs are monotonically assigned.
type Conn struct {
	raw       net.Conn
	mu        sync.Mutex
	corr      atomic.Uint64
	closed    atomic.Bool
	sessionID string
	username  string
	role      string
	features  uint32
}

// Dial opens a TCP (or TLS) connection, performs the handshake, and returns a
// ready-to-use Conn. ctx controls dial + handshake; once that returns, deadlines
// are managed per call by the operation methods.
func Dial(ctx context.Context, opts ConnOptions) (*Conn, error) {
	addr := fmt.Sprintf("%s:%d", opts.Host, opts.Port)
	dialer := &net.Dialer{}
	if opts.DialTimeout > 0 {
		dialer.Timeout = opts.DialTimeout
	}

	var raw net.Conn
	var err error
	if opts.TLS != nil {
		serverName := opts.TLS.ServerName
		if serverName == "" {
			serverName = opts.Host
		}
		cfg := &tls.Config{
			ServerName:         serverName,
			RootCAs:            opts.TLS.RootCAs,
			Certificates:       opts.TLS.Certificates,
			InsecureSkipVerify: opts.TLS.InsecureSkipVerify,
			NextProtos:         []string{"redwire/1"},
		}
		tlsDialer := &tls.Dialer{NetDialer: dialer, Config: cfg}
		raw, err = tlsDialer.DialContext(ctx, "tcp", addr)
	} else {
		raw, err = dialer.DialContext(ctx, "tcp", addr)
	}
	if err != nil {
		return nil, fmt.Errorf("redwire: dial %s: %w", addr, err)
	}

	c := &Conn{raw: raw}
	c.corr.Store(0)

	// Apply ctx deadline to the handshake if one is set.
	if dl, ok := ctx.Deadline(); ok {
		_ = raw.SetDeadline(dl)
	}

	if err := c.handshake(opts); err != nil {
		_ = raw.Close()
		return nil, err
	}
	// Clear deadline so subsequent calls own it.
	_ = raw.SetDeadline(time.Time{})
	return c, nil
}

// SessionID returns the session id the server assigned during AuthOk.
func (c *Conn) SessionID() string { return c.sessionID }

// Username returns the authenticated username (server-side session label).
func (c *Conn) Username() string { return c.username }

// Role returns the authenticated role string.
func (c *Conn) Role() string { return c.role }

// nextCorr returns a monotonically-increasing correlation id, starting at 1.
func (c *Conn) nextCorr() uint64 {
	return c.corr.Add(1)
}

// applyDeadline enforces ctx cancellation on a single read/write step. It
// returns a cleanup func the caller defers.
func (c *Conn) applyDeadline(ctx context.Context) func() {
	if dl, ok := ctx.Deadline(); ok {
		_ = c.raw.SetDeadline(dl)
		return func() { _ = c.raw.SetDeadline(time.Time{}) }
	}
	// Honour ctx.Done() without a deadline by hard-deadlining briefly when ctx
	// is already cancelled. For an unbounded ctx we just clear deadlines.
	_ = c.raw.SetDeadline(time.Time{})
	return func() {}
}

func (c *Conn) writeAll(p []byte) error {
	for len(p) > 0 {
		n, err := c.raw.Write(p)
		if err != nil {
			return err
		}
		p = p[n:]
	}
	return nil
}

func (c *Conn) readFull(p []byte) error {
	_, err := io.ReadFull(c.raw, p)
	return err
}

func (c *Conn) writeFrame(f *Frame) error {
	enc, err := EncodeFrame(f)
	if err != nil {
		return err
	}
	return c.writeAll(enc)
}

func (c *Conn) readFrame() (*Frame, error) {
	var header [FrameHeaderSize]byte
	if err := c.readFull(header[:]); err != nil {
		return nil, err
	}
	length := binary.LittleEndian.Uint32(header[0:4])
	if length < FrameHeaderSize {
		return nil, fmt.Errorf("redwire: server sent length %d", length)
	}
	if length > MaxFrameSize {
		return nil, fmt.Errorf("redwire: server sent oversized frame %d", length)
	}
	buf := make([]byte, length)
	copy(buf[:FrameHeaderSize], header[:])
	if length > FrameHeaderSize {
		if err := c.readFull(buf[FrameHeaderSize:]); err != nil {
			return nil, err
		}
	}
	frame, _, err := DecodeFrame(buf)
	return frame, err
}

// handshake drives the RedWire negotiation: magic + Hello/HelloAck + auth.
func (c *Conn) handshake(opts ConnOptions) error {
	// 1. Magic + minor version.
	if err := c.writeAll([]byte{Magic, SupportedVersion}); err != nil {
		return fmt.Errorf("redwire: write magic: %w", err)
	}

	// 2. Hello.
	methods := authMethodsForCreds(opts.Auth.Method)
	clientName := opts.ClientName
	if clientName == "" {
		clientName = "reddb-go/0.1"
	}
	helloPayload, err := json.Marshal(map[string]any{
		"versions":     []int{1},
		"auth_methods": methods,
		"features":     0,
		"client_name":  clientName,
	})
	if err != nil {
		return fmt.Errorf("redwire: encode hello: %w", err)
	}
	if err := c.writeFrame(NewFrame(KindHello, c.nextCorr(), helloPayload)); err != nil {
		return fmt.Errorf("redwire: write hello: %w", err)
	}

	// 3. Read HelloAck (or AuthFail).
	ack, err := c.readFrame()
	if err != nil {
		return fmt.Errorf("redwire: read hello-ack: %w", err)
	}
	switch ack.Kind {
	case KindHelloAck:
		// fall through
	case KindAuthFail:
		return fmt.Errorf("redwire: auth refused at HelloAck: %s", parseReason(ack.Payload))
	default:
		return fmt.Errorf("redwire: expected HelloAck, got 0x%02x", ack.Kind)
	}
	chosen, err := parseChosenAuth(ack.Payload)
	if err != nil {
		return err
	}

	// 4. AuthResponse for the chosen method.
	switch chosen {
	case "anonymous":
		if err := c.writeFrame(NewFrame(KindAuthResponse, c.nextCorr(), nil)); err != nil {
			return fmt.Errorf("redwire: write auth-response: %w", err)
		}
	case "bearer":
		if opts.Auth.Method != AuthBearer {
			return errors.New("redwire: server demanded bearer but no token was supplied")
		}
		body, _ := json.Marshal(map[string]any{"token": opts.Auth.Token})
		if err := c.writeFrame(NewFrame(KindAuthResponse, c.nextCorr(), body)); err != nil {
			return fmt.Errorf("redwire: write auth-response: %w", err)
		}
	case "oauth-jwt":
		if opts.Auth.Method != AuthOAuthJWT {
			return errors.New("redwire: server demanded oauth-jwt but no JWT was supplied")
		}
		body, _ := json.Marshal(map[string]any{"jwt": opts.Auth.JWT})
		if err := c.writeFrame(NewFrame(KindAuthResponse, c.nextCorr(), body)); err != nil {
			return fmt.Errorf("redwire: write auth-response: %w", err)
		}
	case "scram-sha-256":
		if opts.Auth.Method != AuthScram {
			return errors.New("redwire: server demanded scram-sha-256 but no credentials were supplied")
		}
		if err := c.runScram(opts.Auth.Username, opts.Auth.Password); err != nil {
			return err
		}
	default:
		return fmt.Errorf("redwire: server picked unsupported auth method: %s", chosen)
	}

	// 5. Read AuthOk / AuthFail (SCRAM path consumes its own AuthOk inside
	// runScram and short-circuits here when it returns nil with sessionID set).
	if c.sessionID == "" {
		final, err := c.readFrame()
		if err != nil {
			return fmt.Errorf("redwire: read auth-ok: %w", err)
		}
		switch final.Kind {
		case KindAuthOk:
			c.applyAuthOk(final.Payload)
		case KindAuthFail:
			return fmt.Errorf("redwire: auth refused: %s", parseReason(final.Payload))
		default:
			return fmt.Errorf("redwire: expected AuthOk, got 0x%02x", final.Kind)
		}
	}
	return nil
}

func (c *Conn) runScram(username, password string) error {
	// SCRAM exchange:
	//   client → server: AuthResponse with client-first
	//   server → client: AuthRequest with server-first
	//   client → server: AuthResponse with client-final
	//   server → client: AuthOk (carries v=server-signature) | AuthFail
	sess, err := NewScramSession(username, password)
	if err != nil {
		return err
	}
	body, _ := json.Marshal(map[string]any{"client_first": sess.ClientFirstMessage()})
	if err := c.writeFrame(NewFrame(KindAuthResponse, c.nextCorr(), body)); err != nil {
		return fmt.Errorf("redwire: scram: write client-first: %w", err)
	}
	// Engine ships server-first as the raw `r=...,s=...,i=...` SCRAM payload
	// inside an AuthRequest frame. Some implementations wrap it in JSON
	// {"server_first": ...} — accept either.
	req, err := c.readFrame()
	if err != nil {
		return fmt.Errorf("redwire: scram: read server-first: %w", err)
	}
	if req.Kind == KindAuthFail {
		return fmt.Errorf("redwire: scram refused: %s", parseReason(req.Payload))
	}
	if req.Kind != KindAuthRequest {
		return fmt.Errorf("redwire: scram: expected AuthRequest, got 0x%02x", req.Kind)
	}
	serverFirstRaw, err := unwrapScramServerFirst(req.Payload)
	if err != nil {
		return err
	}
	sf, err := ParseServerFirst(serverFirstRaw)
	if err != nil {
		return err
	}
	final, am, err := sess.BuildClientFinal(sf)
	if err != nil {
		return err
	}
	finalBody, _ := json.Marshal(map[string]any{"client_final": final})
	if err := c.writeFrame(NewFrame(KindAuthResponse, c.nextCorr(), finalBody)); err != nil {
		return fmt.Errorf("redwire: scram: write client-final: %w", err)
	}
	ok, err := c.readFrame()
	if err != nil {
		return fmt.Errorf("redwire: scram: read auth-ok: %w", err)
	}
	switch ok.Kind {
	case KindAuthOk:
		// Verify server signature when present so a forged AuthOk can't pass.
		if v := extractServerSignature(ok.Payload); v != nil {
			if !VerifyServerSignature([]byte(password), sf.Salt, sf.Iter, am, v) {
				return errors.New("redwire: scram: server signature did not verify")
			}
		}
		c.applyAuthOk(ok.Payload)
		return nil
	case KindAuthFail:
		return fmt.Errorf("redwire: scram refused: %s", parseReason(ok.Payload))
	}
	return fmt.Errorf("redwire: scram: expected AuthOk, got 0x%02x", ok.Kind)
}

func unwrapScramServerFirst(payload []byte) ([]byte, error) {
	// Try JSON {"server_first": "..."} first.
	var asObj map[string]any
	if err := json.Unmarshal(payload, &asObj); err == nil {
		if s, ok := asObj["server_first"].(string); ok {
			return []byte(s), nil
		}
	}
	// Otherwise treat the body as the raw SCRAM string.
	return payload, nil
}

// extractServerSignature pulls the `v` (or `server_signature`) field out of an
// AuthOk JSON payload. Returns nil if not present.
func extractServerSignature(payload []byte) []byte {
	var obj map[string]any
	if err := json.Unmarshal(payload, &obj); err != nil {
		return nil
	}
	for _, key := range []string{"v", "server_signature"} {
		if v, ok := obj[key].(string); ok {
			// Try base64 first (engine ships base64), then hex (spec mentions hex).
			if dec, err := DecodeBase64Std(v); err == nil {
				return dec
			}
			if dec, err := decodeHex(v); err == nil {
				return dec
			}
		}
	}
	return nil
}

func decodeHex(s string) ([]byte, error) {
	if len(s)%2 != 0 {
		return nil, errors.New("hex length odd")
	}
	out := make([]byte, len(s)/2)
	for i := 0; i < len(out); i++ {
		hi, err := hexNibble(s[2*i])
		if err != nil {
			return nil, err
		}
		lo, err := hexNibble(s[2*i+1])
		if err != nil {
			return nil, err
		}
		out[i] = (hi << 4) | lo
	}
	return out, nil
}

func hexNibble(b byte) (byte, error) {
	switch {
	case b >= '0' && b <= '9':
		return b - '0', nil
	case b >= 'a' && b <= 'f':
		return b - 'a' + 10, nil
	case b >= 'A' && b <= 'F':
		return b - 'A' + 10, nil
	}
	return 0, errors.New("bad hex char")
}

func (c *Conn) applyAuthOk(payload []byte) {
	var obj map[string]any
	if err := json.Unmarshal(payload, &obj); err != nil {
		return
	}
	if s, ok := obj["session_id"].(string); ok {
		c.sessionID = s
	} else if s, ok := obj["sub"].(string); ok {
		// Spec uses `sub`; engine emits `session_id`. Accept both.
		c.sessionID = s
	}
	if s, ok := obj["username"].(string); ok {
		c.username = s
	}
	if s, ok := obj["role"].(string); ok {
		c.role = s
	}
	if f, ok := obj["features"].(float64); ok {
		c.features = uint32(f)
	}
}

func authMethodsForCreds(m AuthMethod) []string {
	switch m {
	case AuthBearer:
		return []string{"bearer"}
	case AuthScram:
		return []string{"scram-sha-256"}
	case AuthOAuthJWT:
		return []string{"oauth-jwt"}
	default:
		return []string{"anonymous", "bearer"}
	}
}

func parseChosenAuth(payload []byte) (string, error) {
	var obj map[string]any
	if err := json.Unmarshal(payload, &obj); err != nil {
		return "", fmt.Errorf("redwire: decode hello-ack: %w", err)
	}
	if s, ok := obj["auth"].(string); ok {
		return s, nil
	}
	return "", errors.New("redwire: hello-ack missing 'auth' field")
}

func parseReason(payload []byte) string {
	var obj map[string]any
	if err := json.Unmarshal(payload, &obj); err == nil {
		if s, ok := obj["reason"].(string); ok {
			return s
		}
	}
	if len(payload) > 0 {
		return string(payload)
	}
	return "(no reason)"
}

// --- Operations ------------------------------------------------------

// Query sends a SQL string and returns the raw Result.payload bytes.
func (c *Conn) Query(ctx context.Context, sql string) ([]byte, error) {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed.Load() {
		return nil, errors.New("redwire: connection closed")
	}
	cleanup := c.applyDeadline(ctx)
	defer cleanup()
	if err := c.writeFrame(NewFrame(KindQuery, c.nextCorr(), []byte(sql))); err != nil {
		return nil, err
	}
	resp, err := c.readFrame()
	if err != nil {
		return nil, err
	}
	switch resp.Kind {
	case KindResult:
		return resp.Payload, nil
	case KindError:
		return nil, fmt.Errorf("redwire: query: %s", string(resp.Payload))
	}
	return nil, fmt.Errorf("redwire: query: unexpected kind 0x%02x", resp.Kind)
}

// Insert delivers a single row into the named collection.
func (c *Conn) Insert(ctx context.Context, collection string, payload any) error {
	body, err := json.Marshal(map[string]any{
		"collection": collection,
		"payload":    payload,
	})
	if err != nil {
		return fmt.Errorf("redwire: encode insert: %w", err)
	}
	return c.bulkInsertJSON(ctx, body)
}

// BulkInsert delivers a batch of rows.
func (c *Conn) BulkInsert(ctx context.Context, collection string, rows []any) error {
	body, err := json.Marshal(map[string]any{
		"collection": collection,
		"payloads":   rows,
	})
	if err != nil {
		return fmt.Errorf("redwire: encode bulk_insert: %w", err)
	}
	return c.bulkInsertJSON(ctx, body)
}

func (c *Conn) bulkInsertJSON(ctx context.Context, body []byte) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed.Load() {
		return errors.New("redwire: connection closed")
	}
	cleanup := c.applyDeadline(ctx)
	defer cleanup()
	if err := c.writeFrame(NewFrame(KindBulkInsert, c.nextCorr(), body)); err != nil {
		return err
	}
	resp, err := c.readFrame()
	if err != nil {
		return err
	}
	switch resp.Kind {
	case KindBulkOk:
		return nil
	case KindError:
		return fmt.Errorf("redwire: bulk_insert: %s", string(resp.Payload))
	}
	return fmt.Errorf("redwire: bulk_insert: unexpected kind 0x%02x", resp.Kind)
}

// Get fetches one row by id. Returns the raw envelope bytes (`{ok, found, ...}`).
func (c *Conn) Get(ctx context.Context, collection, id string) ([]byte, error) {
	body, _ := json.Marshal(map[string]any{
		"collection": collection,
		"id":         id,
	})
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed.Load() {
		return nil, errors.New("redwire: connection closed")
	}
	cleanup := c.applyDeadline(ctx)
	defer cleanup()
	if err := c.writeFrame(NewFrame(KindGet, c.nextCorr(), body)); err != nil {
		return nil, err
	}
	resp, err := c.readFrame()
	if err != nil {
		return nil, err
	}
	switch resp.Kind {
	case KindResult:
		return resp.Payload, nil
	case KindError:
		return nil, fmt.Errorf("redwire: get: %s", string(resp.Payload))
	}
	return nil, fmt.Errorf("redwire: get: unexpected kind 0x%02x", resp.Kind)
}

// Delete removes one row by id.
func (c *Conn) Delete(ctx context.Context, collection, id string) error {
	body, _ := json.Marshal(map[string]any{
		"collection": collection,
		"id":         id,
	})
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed.Load() {
		return errors.New("redwire: connection closed")
	}
	cleanup := c.applyDeadline(ctx)
	defer cleanup()
	if err := c.writeFrame(NewFrame(KindDelete, c.nextCorr(), body)); err != nil {
		return err
	}
	resp, err := c.readFrame()
	if err != nil {
		return err
	}
	switch resp.Kind {
	case KindDeleteOk:
		return nil
	case KindError:
		return fmt.Errorf("redwire: delete: %s", string(resp.Payload))
	}
	return fmt.Errorf("redwire: delete: unexpected kind 0x%02x", resp.Kind)
}

// Ping sends a Ping and waits for Pong. Cheap keepalive.
func (c *Conn) Ping(ctx context.Context) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	if c.closed.Load() {
		return errors.New("redwire: connection closed")
	}
	cleanup := c.applyDeadline(ctx)
	defer cleanup()
	if err := c.writeFrame(NewFrame(KindPing, c.nextCorr(), nil)); err != nil {
		return err
	}
	resp, err := c.readFrame()
	if err != nil {
		return err
	}
	if resp.Kind != KindPong {
		return fmt.Errorf("redwire: ping: expected Pong, got 0x%02x", resp.Kind)
	}
	return nil
}

// Close sends Bye (best-effort) and tears down the socket.
func (c *Conn) Close() error {
	if !c.closed.CompareAndSwap(false, true) {
		return nil
	}
	c.mu.Lock()
	defer c.mu.Unlock()
	_ = c.writeFrame(NewFrame(KindBye, c.nextCorr(), nil))
	return c.raw.Close()
}
