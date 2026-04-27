// Package httpx is the HTTP / HTTPS transport for the RedDB Go driver. It
// mirrors drivers/js/src/http.js so the REST surface is identical across
// languages.
package httpx

import (
	"bytes"
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"time"
)

// Options configures a Client.
type Options struct {
	BaseURL string
	Token   string
	// Timeout applied to every request. Defaults to 30s when zero.
	Timeout time.Duration
	// RootCAs trust store. nil = system roots.
	RootCAs *x509.CertPool
	// Certificates for mTLS.
	Certificates []tls.Certificate
	// InsecureSkipVerify disables TLS cert verification (dev).
	InsecureSkipVerify bool
	// UserAgent override. Defaults to "reddb-go/0.1".
	UserAgent string
}

// Client is the HTTP transport.
type Client struct {
	base      string
	token     string
	httpc     *http.Client
	userAgent string
}

// NewClient builds a configured *Client; it does not perform any I/O.
func NewClient(opts Options) (*Client, error) {
	if opts.BaseURL == "" {
		return nil, fmt.Errorf("httpx: BaseURL required")
	}
	timeout := opts.Timeout
	if timeout == 0 {
		timeout = 30 * time.Second
	}
	tlsConfig := &tls.Config{
		RootCAs:            opts.RootCAs,
		Certificates:       opts.Certificates,
		InsecureSkipVerify: opts.InsecureSkipVerify,
	}
	transport := &http.Transport{
		TLSClientConfig: tlsConfig,
		// Keep defaults for everything else; users can wrap if they need more.
	}
	ua := opts.UserAgent
	if ua == "" {
		ua = "reddb-go/0.1"
	}
	return &Client{
		base:      trimTrailingSlash(opts.BaseURL),
		token:     opts.Token,
		httpc:     &http.Client{Transport: transport, Timeout: timeout},
		userAgent: ua,
	}, nil
}

// SetToken updates the bearer token used in subsequent requests.
func (c *Client) SetToken(t string) { c.token = t }

// Token returns the currently configured bearer token.
func (c *Client) Token() string { return c.token }

// LoginResult is the body returned by /auth/login.
type LoginResult struct {
	Token string         `json:"token"`
	Body  map[string]any `json:"-"`
}

// Login exchanges username + password for a bearer token, stores it, and
// returns the parsed envelope.
func (c *Client) Login(ctx context.Context, username, password string) (*LoginResult, error) {
	body, _ := json.Marshal(map[string]any{
		"username": username,
		"password": password,
	})
	resp, err := c.do(ctx, http.MethodPost, "/auth/login", body)
	if err != nil {
		return nil, err
	}
	parsed, err := parseEnvelope(resp)
	if err != nil {
		return nil, err
	}
	out := &LoginResult{}
	if obj, ok := parsed.(map[string]any); ok {
		if t, ok := obj["token"].(string); ok {
			out.Token = t
			c.token = t
		}
		out.Body = obj
	}
	return out, nil
}

// Health hits /admin/health (or /health for older builds — we try both).
func (c *Client) Health(ctx context.Context) (any, error) {
	for _, path := range []string{"/admin/health", "/health"} {
		resp, err := c.do(ctx, http.MethodGet, path, nil)
		if err != nil {
			return nil, err
		}
		if resp.StatusCode == http.StatusNotFound {
			_ = resp.Body.Close()
			continue
		}
		return parseEnvelope(resp)
	}
	return nil, fmt.Errorf("httpx: no health endpoint responded")
}

// Query sends a SQL string. Returns the parsed envelope (`{records: [...]}` or
// the engine's wrapper).
func (c *Client) Query(ctx context.Context, sql string) (any, error) {
	body, _ := json.Marshal(map[string]any{"query": sql})
	resp, err := c.do(ctx, http.MethodPost, "/query", body)
	if err != nil {
		return nil, err
	}
	return parseEnvelope(resp)
}

// Insert posts a single row to the engine via the same /query endpoint shape
// the JS driver uses. We hit /collections/:name/rows which the engine routes
// to its insert handler.
func (c *Client) Insert(ctx context.Context, collection string, payload any) (any, error) {
	body, err := json.Marshal(payload)
	if err != nil {
		return nil, fmt.Errorf("httpx: encode insert: %w", err)
	}
	resp, err := c.do(ctx, http.MethodPost,
		fmt.Sprintf("/collections/%s/rows", url.PathEscape(collection)),
		body)
	if err != nil {
		return nil, err
	}
	return parseEnvelope(resp)
}

// BulkInsert delivers a batch of rows.
func (c *Client) BulkInsert(ctx context.Context, collection string, payloads []any) (any, error) {
	body, err := json.Marshal(map[string]any{"rows": payloads})
	if err != nil {
		return nil, fmt.Errorf("httpx: encode bulk_insert: %w", err)
	}
	resp, err := c.do(ctx, http.MethodPost,
		fmt.Sprintf("/collections/%s/bulk/rows", url.PathEscape(collection)),
		body)
	if err != nil {
		return nil, err
	}
	return parseEnvelope(resp)
}

// Scan posts an ad-hoc scan request and returns the parsed envelope.
func (c *Client) Scan(ctx context.Context, params map[string]any) (any, error) {
	body, err := json.Marshal(params)
	if err != nil {
		return nil, fmt.Errorf("httpx: encode scan: %w", err)
	}
	resp, err := c.do(ctx, http.MethodPost, "/scan", body)
	if err != nil {
		return nil, err
	}
	return parseEnvelope(resp)
}

// Get fetches one record by id.
func (c *Client) Get(ctx context.Context, collection, id string) (any, error) {
	resp, err := c.do(ctx, http.MethodGet,
		fmt.Sprintf("/collections/%s/%s",
			url.PathEscape(collection), url.PathEscape(id)),
		nil)
	if err != nil {
		return nil, err
	}
	return parseEnvelope(resp)
}

// Delete removes one record by id and returns the affected count (when the
// engine supplies it).
func (c *Client) Delete(ctx context.Context, collection, id string) (any, error) {
	resp, err := c.do(ctx, http.MethodDelete,
		fmt.Sprintf("/collections/%s/%s",
			url.PathEscape(collection), url.PathEscape(id)),
		nil)
	if err != nil {
		return nil, err
	}
	return parseEnvelope(resp)
}

// Close releases resources. The HTTP client is stateless, but we expose the
// method so callers can use httpx.Client behind an io.Closer interface.
func (c *Client) Close() error {
	c.httpc.CloseIdleConnections()
	return nil
}

// --- internals -------------------------------------------------------

func (c *Client) do(ctx context.Context, method, path string, body []byte) (*http.Response, error) {
	url := c.base + path
	var reader io.Reader
	if body != nil {
		reader = bytes.NewReader(body)
	}
	req, err := http.NewRequestWithContext(ctx, method, url, reader)
	if err != nil {
		return nil, fmt.Errorf("httpx: build request: %w", err)
	}
	if body != nil {
		req.Header.Set("content-type", "application/json")
	}
	req.Header.Set("user-agent", c.userAgent)
	if c.token != "" {
		req.Header.Set("authorization", "Bearer "+c.token)
	}
	resp, err := c.httpc.Do(req)
	if err != nil {
		return nil, fmt.Errorf("httpx: %s %s: %w", method, path, err)
	}
	return resp, nil
}

func parseEnvelope(resp *http.Response) (any, error) {
	defer resp.Body.Close()
	bs, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, fmt.Errorf("httpx: read body: %w", err)
	}
	var body any
	if len(bs) > 0 {
		if err := json.Unmarshal(bs, &body); err != nil {
			body = string(bs)
		}
	}
	if resp.StatusCode >= 400 {
		msg := fmt.Sprintf("status %d", resp.StatusCode)
		if obj, ok := body.(map[string]any); ok {
			if s, ok := obj["error"].(string); ok {
				msg = s
			} else if s, ok := obj["message"].(string); ok {
				msg = s
			}
		} else if s, ok := body.(string); ok {
			msg = s
		}
		return nil, fmt.Errorf("httpx: %d: %s", resp.StatusCode, msg)
	}
	// RedDB envelope: { ok, result, error? }. Unwrap when present.
	if obj, ok := body.(map[string]any); ok {
		if okVal, present := obj["ok"]; present {
			if b, _ := okVal.(bool); !b {
				msg, _ := obj["error"].(string)
				if msg == "" {
					msg = "server returned ok=false"
				}
				return nil, fmt.Errorf("httpx: %s", msg)
			}
			if r, present := obj["result"]; present {
				return r, nil
			}
		}
	}
	return body, nil
}

func trimTrailingSlash(s string) string {
	for len(s) > 0 && s[len(s)-1] == '/' {
		s = s[:len(s)-1]
	}
	return s
}
