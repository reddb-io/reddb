package reddb

import (
	"fmt"
	"net/url"
	"strconv"
	"strings"
)

// Kind enumerates the transports a parsed URI can resolve to. Mirrors
// drivers/js/src/url.js so the URI grammar is the same on every driver.
type Kind string

const (
	KindEmbedded Kind = "embedded"
	KindRedWire  Kind = "redwire"
	KindRedWires Kind = "redwires"
	KindHTTP     Kind = "http"
	KindHTTPS    Kind = "https"
)

// ParsedURI is the normalised representation of a connection string. Only the
// fields that apply to the resolved Kind are populated.
type ParsedURI struct {
	Kind     Kind
	Host     string
	Port     int
	Path     string
	Username string
	Password string
	Token    string
	APIKey   string
	LoginURL string
	Params   url.Values
	Original string
}

// ParseURI normalises any connection string the driver accepts.
//
//	red://[user[:pass]@]host[:port]/?...   plain RedWire (default port 5050)
//	reds://...                              RedWire over TLS
//	http://...   https://...                HTTP / HTTPS REST
//	red:///path/to/file.rdb                 embedded (not yet supported)
//	red://memory  red://:memory  red://:memory:   embedded in-memory (not yet)
func ParseURI(uri string) (*ParsedURI, error) {
	if uri == "" {
		return nil, NewError(CodeUnparseableURI, "uri is empty")
	}

	switch {
	case isRedScheme(uri):
		return parseRedURI(uri)
	case strings.HasPrefix(uri, "reds://"):
		return parseRemoteURI(uri, KindRedWires, defaultPortFor(KindRedWires))
	case strings.HasPrefix(uri, "http://"):
		return parseRemoteURI(uri, KindHTTP, defaultPortFor(KindHTTP))
	case strings.HasPrefix(uri, "https://"):
		return parseRemoteURI(uri, KindHTTPS, defaultPortFor(KindHTTPS))
	}

	return nil, NewError(CodeUnsupportedScheme, fmt.Sprintf("unsupported uri: %q", uri))
}

func isRedScheme(uri string) bool {
	return strings.HasPrefix(uri, "red://") || uri == "red:" || uri == "red:/"
}

// parseRedURI handles every red:// shape, including embedded variants.
func parseRedURI(uri string) (*ParsedURI, error) {
	// `red://memory`, `red://:memory`, `red://:memory:` are all aliases for
	// embedded in-memory. `red:///path` is embedded with a filesystem path.
	switch uri {
	case "red:", "red:/", "red://", "red://memory", "red://memory/",
		"red://:memory", "red://:memory:":
		return nil, NewError(CodeEmbeddedUnsupported,
			"embedded in-memory mode requires a future cgo build of the driver")
	}
	if strings.HasPrefix(uri, "red:///") {
		return nil, NewError(CodeEmbeddedUnsupported,
			"embedded file mode requires a future cgo build of the driver")
	}

	// Strip `red://` and parse what's left as `host[:port]/path?query`.
	rest := strings.TrimPrefix(uri, "red://")
	parsed, err := url.Parse("red://" + rest)
	if err != nil {
		return nil, WrapError(CodeUnparseableURI, "failed to parse uri", err)
	}

	params := parsed.Query()
	proto := strings.ToLower(params.Get("proto"))
	kind, err := resolveKind(proto)
	if err != nil {
		return nil, err
	}

	port := 0
	if parsed.Port() != "" {
		n, perr := strconv.Atoi(parsed.Port())
		if perr != nil {
			return nil, WrapError(CodeUnparseableURI, "invalid port", perr)
		}
		port = n
	} else {
		port = defaultPortFor(kind)
	}

	path := ""
	if parsed.Path != "" && parsed.Path != "/" {
		path = parsed.Path
	}

	out := &ParsedURI{
		Kind:     kind,
		Host:     parsed.Hostname(),
		Port:     port,
		Path:     path,
		Token:    firstNonEmpty(params.Get("token")),
		APIKey:   firstNonEmpty(params.Get("apiKey"), params.Get("api_key")),
		LoginURL: firstNonEmpty(params.Get("loginUrl"), params.Get("login_url")),
		Params:   params,
		Original: uri,
	}
	if u := parsed.User; u != nil {
		out.Username = u.Username()
		if pw, ok := u.Password(); ok {
			out.Password = pw
		}
	}
	if out.Host == "" {
		return nil, NewError(CodeUnparseableURI,
			fmt.Sprintf("uri missing host: %q", uri))
	}
	return out, nil
}

// parseRemoteURI handles reds://, http://, https://. The kind is fixed at the
// scheme level; query params still drive token / apiKey / loginUrl.
func parseRemoteURI(uri string, kind Kind, defaultPort int) (*ParsedURI, error) {
	parsed, err := url.Parse(uri)
	if err != nil {
		return nil, WrapError(CodeUnparseableURI, "failed to parse uri", err)
	}
	port := defaultPort
	if parsed.Port() != "" {
		n, perr := strconv.Atoi(parsed.Port())
		if perr != nil {
			return nil, WrapError(CodeUnparseableURI, "invalid port", perr)
		}
		port = n
	}
	host := parsed.Hostname()
	if host == "" {
		return nil, NewError(CodeUnparseableURI,
			fmt.Sprintf("uri missing host: %q", uri))
	}
	params := parsed.Query()
	path := ""
	if parsed.Path != "" && parsed.Path != "/" {
		path = parsed.Path
	}

	out := &ParsedURI{
		Kind:     kind,
		Host:     host,
		Port:     port,
		Path:     path,
		Token:    firstNonEmpty(params.Get("token")),
		APIKey:   firstNonEmpty(params.Get("apiKey"), params.Get("api_key")),
		LoginURL: firstNonEmpty(params.Get("loginUrl"), params.Get("login_url")),
		Params:   params,
		Original: uri,
	}
	if u := parsed.User; u != nil {
		out.Username = u.Username()
		if pw, ok := u.Password(); ok {
			out.Password = pw
		}
	}
	return out, nil
}

func resolveKind(proto string) (Kind, error) {
	switch proto {
	case "", "red", "redwire":
		return KindRedWire, nil
	case "reds", "redwires":
		return KindRedWires, nil
	case "http":
		return KindHTTP, nil
	case "https":
		return KindHTTPS, nil
	default:
		return "", NewError(CodeUnsupportedProto,
			fmt.Sprintf("unknown proto=%q. Supported: red | reds | http | https", proto))
	}
}

func defaultPortFor(k Kind) int {
	switch k {
	case KindRedWire, KindRedWires:
		return 5050
	case KindHTTP:
		return 8080
	case KindHTTPS:
		return 8443
	}
	return 0
}

func firstNonEmpty(values ...string) string {
	for _, v := range values {
		if v != "" {
			return v
		}
	}
	return ""
}

// HTTPBaseURL renders the parsed URI as the base URL used by httpx.Client. Only
// valid for HTTP / HTTPS kinds — other transports return an empty string.
func (p *ParsedURI) HTTPBaseURL() string {
	switch p.Kind {
	case KindHTTP:
		return fmt.Sprintf("http://%s:%d", p.Host, p.Port)
	case KindHTTPS:
		return fmt.Sprintf("https://%s:%d", p.Host, p.Port)
	}
	return ""
}

// DeriveLoginURL returns the explicit `loginUrl=` override if set, otherwise
// `<scheme>://host:port/auth/login` derived from an HTTP-shaped URI. RedWire
// kinds default to https://host/auth/login because RedWire itself doesn't
// expose login.
func (p *ParsedURI) DeriveLoginURL() (string, error) {
	if p.LoginURL != "" {
		return p.LoginURL, nil
	}
	if p.Host == "" {
		return "", NewError(CodeUnparseableURI,
			"cannot derive loginUrl without a host; pass loginUrl=...")
	}
	switch p.Kind {
	case KindHTTP, KindHTTPS:
		return fmt.Sprintf("%s://%s:%d/auth/login", string(p.Kind), p.Host, p.Port), nil
	}
	return fmt.Sprintf("https://%s/auth/login", p.Host), nil
}
