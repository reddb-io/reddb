package reddb

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"fmt"
	"time"

	"github.com/forattini-dev/reddb-go/httpx"
	"github.com/forattini-dev/reddb-go/redwire"
)

// Conn is the transport-agnostic connection interface every Connect call
// returns. The same set of operations works whether the underlying transport
// is RedWire (binary TCP) or HTTP REST.
type Conn interface {
	// Query runs a SQL string and returns the raw JSON-encoded payload bytes
	// (the engine's `Result.payload`). Callers decode them however they like.
	Query(ctx context.Context, sql string) ([]byte, error)
	// Insert delivers a single row.
	Insert(ctx context.Context, collection string, payload any) error
	// BulkInsert delivers a batch of rows.
	BulkInsert(ctx context.Context, collection string, rows []any) error
	// Get fetches one row by id and returns the raw envelope bytes.
	Get(ctx context.Context, collection, id string) ([]byte, error)
	// Delete removes one row by id.
	Delete(ctx context.Context, collection, id string) error
	// Ping is a cheap keepalive that round-trips a Ping/Pong frame (or hits
	// /admin/health for HTTP transports).
	Ping(ctx context.Context) error
	// Close releases the underlying socket / connection pool. Safe to call
	// multiple times; subsequent calls return nil.
	Close() error
}

// Options tweaks the Connect behaviour.
type Options struct {
	// ClientName is sent in the RedWire Hello payload. Defaults to
	// `reddb-go/0.1`.
	ClientName string
	// DialTimeout caps the TCP dial. Zero means no driver-imposed cap (the OS
	// retains its own).
	DialTimeout time.Duration
	// HTTPTimeout caps every HTTP request. Zero defaults to 30s.
	HTTPTimeout time.Duration
	// TLSRootCAs trusted CA bundle. nil = system roots.
	TLSRootCAs *x509.CertPool
	// TLSCertificates for mTLS.
	TLSCertificates []tls.Certificate
	// TLSInsecureSkipVerify disables TLS cert verification (dev only).
	TLSInsecureSkipVerify bool
	// TLSServerName overrides SNI / cert-name validation.
	TLSServerName string
	// Token to inject as the bearer (HTTP) or RedWire bearer auth.
	Token string
	// Username + Password trigger SCRAM (RedWire) or HTTP /auth/login flow.
	Username string
	Password string
	// JWT is forwarded as oauth-jwt over RedWire when set.
	JWT string
}

// Option is the functional-option form callers can pass to Connect.
type Option func(*Options)

// WithClientName overrides the default client name.
func WithClientName(name string) Option {
	return func(o *Options) { o.ClientName = name }
}

// WithToken sets a pre-issued bearer token.
func WithToken(token string) Option {
	return func(o *Options) { o.Token = token }
}

// WithBasicAuth supplies username + password for SCRAM (RedWire) or login (HTTP).
func WithBasicAuth(username, password string) Option {
	return func(o *Options) {
		o.Username = username
		o.Password = password
	}
}

// WithJWT supplies an OAuth JWT for RedWire oauth-jwt auth.
func WithJWT(jwt string) Option {
	return func(o *Options) { o.JWT = jwt }
}

// WithDialTimeout caps the dial time.
func WithDialTimeout(d time.Duration) Option {
	return func(o *Options) { o.DialTimeout = d }
}

// WithHTTPTimeout caps per-request HTTP timeouts.
func WithHTTPTimeout(d time.Duration) Option {
	return func(o *Options) { o.HTTPTimeout = d }
}

// WithTLSConfig threads custom TLS material through the dial.
func WithTLSConfig(rootCAs *x509.CertPool, certs []tls.Certificate, serverName string, insecure bool) Option {
	return func(o *Options) {
		o.TLSRootCAs = rootCAs
		o.TLSCertificates = certs
		o.TLSServerName = serverName
		o.TLSInsecureSkipVerify = insecure
	}
}

// Connect parses the URI, dials the appropriate transport, and returns a Conn.
//
//	red://host:5050              RedWire (default port 5050)
//	reds://host:5050             RedWire over TLS
//	http://host:8080             HTTP
//	https://host:8443            HTTPS
//
// Embedded URIs (`red:///path` or `red://memory`) are not supported by the
// pure-Go driver; ParseURI returns CodeEmbeddedUnsupported for those.
func Connect(ctx context.Context, uri string, opts ...Option) (Conn, error) {
	o := Options{}
	for _, fn := range opts {
		fn(&o)
	}

	parsed, err := ParseURI(uri)
	if err != nil {
		return nil, err
	}
	// URI-level credentials take precedence when the explicit option wasn't set.
	if o.Token == "" && parsed.Token != "" {
		o.Token = parsed.Token
	}
	if o.Token == "" && parsed.APIKey != "" {
		o.Token = parsed.APIKey
	}
	if o.Username == "" && parsed.Username != "" {
		o.Username = parsed.Username
	}
	if o.Password == "" && parsed.Password != "" {
		o.Password = parsed.Password
	}

	switch parsed.Kind {
	case KindRedWire, KindRedWires:
		return connectRedWire(ctx, parsed, &o)
	case KindHTTP, KindHTTPS:
		return connectHTTP(ctx, parsed, &o)
	case KindEmbedded:
		return nil, NewError(CodeEmbeddedUnsupported,
			"embedded mode requires a future cgo build")
	}
	return nil, NewError(CodeUnsupportedScheme,
		fmt.Sprintf("kind %q has no transport", parsed.Kind))
}

// --- RedWire facade ---------------------------------------------------

func connectRedWire(ctx context.Context, p *ParsedURI, o *Options) (Conn, error) {
	auth := redwire.AuthCreds{Method: redwire.AuthAnonymous}
	switch {
	case o.JWT != "":
		auth = redwire.AuthCreds{Method: redwire.AuthOAuthJWT, JWT: o.JWT}
	case o.Username != "" && o.Password != "":
		auth = redwire.AuthCreds{
			Method:   redwire.AuthScram,
			Username: o.Username,
			Password: o.Password,
		}
	case o.Token != "":
		auth = redwire.AuthCreds{Method: redwire.AuthBearer, Token: o.Token}
	}

	connOpts := redwire.ConnOptions{
		Host:        p.Host,
		Port:        p.Port,
		Auth:        auth,
		ClientName:  o.ClientName,
		DialTimeout: o.DialTimeout,
	}
	if p.Kind == KindRedWires {
		connOpts.TLS = &redwire.TLSConfig{
			ServerName:         o.TLSServerName,
			RootCAs:            o.TLSRootCAs,
			Certificates:       o.TLSCertificates,
			InsecureSkipVerify: o.TLSInsecureSkipVerify,
		}
	}
	c, err := redwire.Dial(ctx, connOpts)
	if err != nil {
		return nil, WrapError(CodeNetwork, "redwire dial", err)
	}
	return &redwireFacade{conn: c}, nil
}

type redwireFacade struct {
	conn *redwire.Conn
}

func (r *redwireFacade) Query(ctx context.Context, sql string) ([]byte, error) {
	return r.conn.Query(ctx, sql)
}
func (r *redwireFacade) Insert(ctx context.Context, collection string, payload any) error {
	return r.conn.Insert(ctx, collection, payload)
}
func (r *redwireFacade) BulkInsert(ctx context.Context, collection string, rows []any) error {
	return r.conn.BulkInsert(ctx, collection, rows)
}
func (r *redwireFacade) Get(ctx context.Context, collection, id string) ([]byte, error) {
	return r.conn.Get(ctx, collection, id)
}
func (r *redwireFacade) Delete(ctx context.Context, collection, id string) error {
	return r.conn.Delete(ctx, collection, id)
}
func (r *redwireFacade) Ping(ctx context.Context) error { return r.conn.Ping(ctx) }
func (r *redwireFacade) Close() error                   { return r.conn.Close() }

// --- HTTP facade ------------------------------------------------------

func connectHTTP(ctx context.Context, p *ParsedURI, o *Options) (Conn, error) {
	c, err := httpx.NewClient(httpx.Options{
		BaseURL:            p.HTTPBaseURL(),
		Token:              o.Token,
		Timeout:            o.HTTPTimeout,
		RootCAs:            o.TLSRootCAs,
		Certificates:       o.TLSCertificates,
		InsecureSkipVerify: o.TLSInsecureSkipVerify,
		UserAgent:          o.ClientName,
	})
	if err != nil {
		return nil, WrapError(CodeNetwork, "http client", err)
	}
	if o.Token == "" && o.Username != "" && o.Password != "" {
		if _, err := c.Login(ctx, o.Username, o.Password); err != nil {
			return nil, WrapError(CodeAuthRefused, "http login", err)
		}
	}
	return &httpFacade{c: c}, nil
}

type httpFacade struct{ c *httpx.Client }

func (h *httpFacade) Query(ctx context.Context, sql string) ([]byte, error) {
	out, err := h.c.Query(ctx, sql)
	if err != nil {
		return nil, err
	}
	return jsonBytes(out)
}
func (h *httpFacade) Insert(ctx context.Context, collection string, payload any) error {
	_, err := h.c.Insert(ctx, collection, payload)
	return err
}
func (h *httpFacade) BulkInsert(ctx context.Context, collection string, rows []any) error {
	_, err := h.c.BulkInsert(ctx, collection, rows)
	return err
}
func (h *httpFacade) Get(ctx context.Context, collection, id string) ([]byte, error) {
	out, err := h.c.Get(ctx, collection, id)
	if err != nil {
		return nil, err
	}
	return jsonBytes(out)
}
func (h *httpFacade) Delete(ctx context.Context, collection, id string) error {
	_, err := h.c.Delete(ctx, collection, id)
	return err
}
func (h *httpFacade) Ping(ctx context.Context) error {
	_, err := h.c.Health(ctx)
	return err
}
func (h *httpFacade) Close() error { return h.c.Close() }

func jsonBytes(v any) ([]byte, error) {
	if v == nil {
		return nil, nil
	}
	if bs, ok := v.([]byte); ok {
		return bs, nil
	}
	return json.Marshal(v)
}
