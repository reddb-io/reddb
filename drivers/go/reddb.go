package reddb

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"strconv"
	"time"

	"github.com/reddb-io/reddb-go/grpcx"
	"github.com/reddb-io/reddb-go/httpx"
	"github.com/reddb-io/reddb-go/redwire"
)

// Conn is the transport-agnostic connection interface every Connect call
// returns. Query and Ping work across RedWire, HTTP REST, and gRPC; mutation
// helpers are wired for RedWire and HTTP today.
type Conn interface {
	// Query runs a SQL string and returns the raw JSON-encoded payload bytes
	// (the engine's `Result.payload`). Callers decode them however they like.
	//
	// Optional `params` carry positional `$N` bind values. The variadic form is
	// backwards-compatible — `Query(ctx, sql)` keeps its byte-identical wire
	// path. When params are present, RedWire routes through the binary
	// `QueryWithParams` frame (0x28) and requires the server to advertise
	// `FeatureParams`; HTTP forwards the typed JSON `params` array to
	// `/query`.
	//
	// Native Go type mapping (see `redwire.EncodeValue` for the full list):
	//   nil                    -> Null
	//   bool                   -> Bool
	//   intN / uintN           -> Int (i64)
	//   float32 / float64      -> Float (f64)
	//   string                 -> Text
	//   []byte                 -> Bytes
	//   []float32 / []float64  -> Vector
	//   time.Time              -> Timestamp (unix seconds)
	//   redwire.UUID           -> Uuid
	//   map[string]any         -> Json (canonical bytes)
	Query(ctx context.Context, sql string, params ...any) ([]byte, error)
	// Exec runs a SQL statement with the same parameter binding rules as Query
	// and returns a compact mutation result.
	Exec(ctx context.Context, sql string, params ...any) (Result, error)
	// Insert delivers a single row.
	Insert(ctx context.Context, collection string, payload any) error
	// BulkInsert delivers a batch of rows.
	BulkInsert(ctx context.Context, collection string, rows []any) (*BulkInsertResult, error)
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

// BulkInsertResult is returned by bulk row inserts when the transport exposes
// the server envelope.
type BulkInsertResult struct {
	Affected uint64   `json:"affected"`
	IDs      []string `json:"ids,omitempty"`
}

// Result is returned by Exec.
type Result struct {
	Affected uint64          `json:"affected"`
	Raw      json.RawMessage `json:"-"`
}

// RowsAffected reports the number of rows changed by the statement, when the
// server supplied one.
func (r Result) RowsAffected() uint64 { return r.Affected }

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
//	grpc://host:5055             gRPC
//	grpcs://host:5055            gRPC over TLS
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
	case KindGRPC, KindGRPCS:
		return connectGRPC(ctx, parsed, &o)
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

func (r *redwireFacade) Query(ctx context.Context, sql string, params ...any) ([]byte, error) {
	body, err := r.conn.Query(ctx, sql, params...)
	if err != nil {
		if errors.Is(err, redwire.ErrParamsUnsupported) {
			return nil, NewError(CodeParamsUnsupported, err.Error())
		}
		return nil, err
	}
	return body, nil
}
func (r *redwireFacade) Exec(ctx context.Context, sql string, params ...any) (Result, error) {
	body, err := r.Query(ctx, sql, params...)
	if err != nil {
		return Result{}, err
	}
	return execResultFromJSON(body)
}
func (r *redwireFacade) Insert(ctx context.Context, collection string, payload any) error {
	return r.conn.Insert(ctx, collection, payload)
}
func (r *redwireFacade) BulkInsert(ctx context.Context, collection string, rows []any) (*BulkInsertResult, error) {
	result, err := r.conn.BulkInsert(ctx, collection, rows)
	if err != nil {
		return nil, err
	}
	return &BulkInsertResult{Affected: result.Affected, IDs: result.IDs}, nil
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

func (h *httpFacade) Query(ctx context.Context, sql string, params ...any) ([]byte, error) {
	httpParams, err := convertParamsForHTTP(params)
	if err != nil {
		return nil, NewError(CodeParamsUnsupported, err.Error())
	}
	out, err := h.c.Query(ctx, sql, httpParams...)
	if err != nil {
		return nil, err
	}
	return jsonBytes(out)
}
func (h *httpFacade) Exec(ctx context.Context, sql string, params ...any) (Result, error) {
	body, err := h.Query(ctx, sql, params...)
	if err != nil {
		return Result{}, err
	}
	return execResultFromJSON(body)
}

// convertParamsForHTTP lifts top-level `redwire.UUID` params into the
// httpx-local UUID so the HTTP transport doesn't need a redwire dependency.
// Other Go types pass through unchanged.
func convertParamsForHTTP(params []any) ([]any, error) {
	if len(params) == 0 {
		return nil, nil
	}
	out := make([]any, len(params))
	for i, p := range params {
		switch v := p.(type) {
		case redwire.UUID:
			out[i] = httpx.UUID(v)
		default:
			out[i] = v
		}
	}
	return out, nil
}
func (h *httpFacade) Insert(ctx context.Context, collection string, payload any) error {
	_, err := h.c.Insert(ctx, collection, payload)
	return err
}
func (h *httpFacade) BulkInsert(ctx context.Context, collection string, rows []any) (*BulkInsertResult, error) {
	out, err := h.c.BulkInsert(ctx, collection, rows)
	if err != nil {
		return nil, err
	}
	return parseBulkInsertResult(out)
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

func execResultFromJSON(body []byte) (Result, error) {
	result := Result{Raw: append(json.RawMessage(nil), body...)}
	if len(body) == 0 {
		return result, nil
	}
	var obj map[string]any
	if err := json.Unmarshal(body, &obj); err != nil {
		return Result{}, err
	}
	result.Affected = affectedFromMap(obj)
	if result.Affected == 0 {
		if nested, ok := obj["result"].(map[string]any); ok {
			result.Affected = affectedFromMap(nested)
		}
	}
	return result, nil
}

func affectedFromMap(obj map[string]any) uint64 {
	for _, key := range []string{"affected_rows", "affected"} {
		switch v := obj[key].(type) {
		case float64:
			if v > 0 {
				return uint64(v)
			}
		case json.Number:
			n, _ := v.Int64()
			if n > 0 {
				return uint64(n)
			}
		}
	}
	return 0
}

func parseBulkInsertResult(v any) (*BulkInsertResult, error) {
	obj, ok := v.(map[string]any)
	if !ok {
		bs, err := jsonBytes(v)
		if err != nil {
			return nil, err
		}
		if len(bs) == 0 {
			return &BulkInsertResult{}, nil
		}
		if err := json.Unmarshal(bs, &obj); err != nil {
			return nil, err
		}
	}
	result := &BulkInsertResult{}
	switch affected := obj["affected"].(type) {
	case float64:
		result.Affected = uint64(affected)
	case json.Number:
		n, _ := affected.Int64()
		result.Affected = uint64(n)
	}
	if rawIDs, ok := obj["ids"].([]any); ok {
		result.IDs = make([]string, 0, len(rawIDs))
		for _, raw := range rawIDs {
			switch id := raw.(type) {
			case string:
				result.IDs = append(result.IDs, id)
			case float64:
				result.IDs = append(result.IDs, strconv.FormatUint(uint64(id), 10))
			case json.Number:
				result.IDs = append(result.IDs, id.String())
			}
		}
	}
	return result, nil
}

// --- gRPC facade ------------------------------------------------------

func connectGRPC(ctx context.Context, p *ParsedURI, o *Options) (Conn, error) {
	c, err := grpcx.Dial(ctx, grpcx.Options{
		Addr:                  net.JoinHostPort(p.Host, strconv.Itoa(p.Port)),
		Token:                 o.Token,
		Timeout:               o.DialTimeout,
		TLSRootCAs:            o.TLSRootCAs,
		TLSCertificates:       o.TLSCertificates,
		TLSInsecureSkipVerify: o.TLSInsecureSkipVerify,
		TLSServerName:         o.TLSServerName,
		Plaintext:             p.Kind == KindGRPC,
	})
	if err != nil {
		return nil, WrapError(CodeNetwork, "grpc dial", err)
	}
	return &grpcFacade{c: c}, nil
}

type grpcFacade struct{ c *grpcx.Client }

func (g *grpcFacade) Query(ctx context.Context, sql string, params ...any) ([]byte, error) {
	reply, err := g.c.Query(ctx, sql, params...)
	if err != nil {
		return nil, WrapError(CodeEngine, "grpc query", err)
	}
	return []byte(reply.GetResultJson()), nil
}

func (g *grpcFacade) Exec(ctx context.Context, sql string, params ...any) (Result, error) {
	body, err := g.Query(ctx, sql, params...)
	if err != nil {
		return Result{}, err
	}
	return execResultFromJSON(body)
}

func (g *grpcFacade) Insert(context.Context, string, any) error {
	return NewError(CodeProtocol, "grpc Insert is not implemented in the Go driver")
}

func (g *grpcFacade) BulkInsert(context.Context, string, []any) (*BulkInsertResult, error) {
	return nil, NewError(CodeProtocol, "grpc BulkInsert is not implemented in the Go driver")
}

func (g *grpcFacade) Get(context.Context, string, string) ([]byte, error) {
	return nil, NewError(CodeProtocol, "grpc Get is not implemented in the Go driver")
}

func (g *grpcFacade) Delete(context.Context, string, string) error {
	return NewError(CodeProtocol, "grpc Delete is not implemented in the Go driver")
}

func (g *grpcFacade) Ping(ctx context.Context) error { return g.c.Ping(ctx) }
func (g *grpcFacade) Close() error                   { return g.c.Close() }
