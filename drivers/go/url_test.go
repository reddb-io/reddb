package reddb

import "testing"

func TestParseURI_TableDriven(t *testing.T) {
	// errCode == "" means parsing must succeed and the want fields must match.
	type want struct {
		kind     Kind
		host     string
		port     int
		path     string
		username string
		password string
		token    string
		apiKey   string
	}

	cases := []struct {
		name    string
		uri     string
		errCode ErrorCode
		w       want
	}{
		// happy path: plain redwire
		{name: "red plain default port",
			uri: "red://localhost",
			w:   want{kind: KindRedWire, host: "localhost", port: 5050}},
		{name: "red plain explicit port",
			uri: "red://localhost:6000",
			w:   want{kind: KindRedWire, host: "localhost", port: 6000}},
		{name: "red plain ipv4",
			uri: "red://127.0.0.1:5050",
			w:   want{kind: KindRedWire, host: "127.0.0.1", port: 5050}},
		{name: "red with user",
			uri: "red://user@host:5050",
			w:   want{kind: KindRedWire, host: "host", port: 5050, username: "user"}},
		{name: "red with user/pass",
			uri: "red://u:p@host:5050",
			w: want{kind: KindRedWire, host: "host", port: 5050,
				username: "u", password: "p"}},
		{name: "red token query",
			uri: "red://host?token=abc",
			w:   want{kind: KindRedWire, host: "host", port: 5050, token: "abc"}},
		{name: "red apiKey query",
			uri: "red://host?apiKey=ak1",
			w:   want{kind: KindRedWire, host: "host", port: 5050, apiKey: "ak1"}},
		{name: "red apiKey snake_case",
			uri: "red://host?api_key=ak2",
			w:   want{kind: KindRedWire, host: "host", port: 5050, apiKey: "ak2"}},

		// proto override
		{name: "red proto=reds",
			uri: "red://host?proto=reds",
			w:   want{kind: KindRedWires, host: "host", port: 5050}},
		{name: "red proto=https default port",
			uri: "red://host?proto=https",
			w:   want{kind: KindHTTPS, host: "host", port: 8443}},
		{name: "red proto=http default port",
			uri: "red://host?proto=http",
			w:   want{kind: KindHTTP, host: "host", port: 8080}},
		{name: "red proto=red explicit",
			uri: "red://host?proto=red",
			w:   want{kind: KindRedWire, host: "host", port: 5050}},

		// reds:// / http:// / https://
		{name: "reds default port",
			uri: "reds://host",
			w:   want{kind: KindRedWires, host: "host", port: 5050}},
		{name: "http default port",
			uri: "http://host",
			w:   want{kind: KindHTTP, host: "host", port: 8080}},
		{name: "https default port",
			uri: "https://host",
			w:   want{kind: KindHTTPS, host: "host", port: 8443}},
		{name: "https with creds",
			uri: "https://u:p@host:9000",
			w: want{kind: KindHTTPS, host: "host", port: 9000,
				username: "u", password: "p"}},
		{name: "http path preserved",
			uri: "http://host:8080/api/v1",
			w:   want{kind: KindHTTP, host: "host", port: 8080, path: "/api/v1"}},

		// errors
		{name: "empty uri", uri: "", errCode: CodeUnparseableURI},
		{name: "unsupported scheme", uri: "ftp://host", errCode: CodeUnsupportedScheme},
		{name: "unsupported proto", uri: "red://host?proto=quic",
			errCode: CodeUnsupportedProto},
		{name: "embedded in-memory", uri: "red://memory",
			errCode: CodeEmbeddedUnsupported},
		{name: "embedded sqlite-style", uri: "red://:memory:",
			errCode: CodeEmbeddedUnsupported},
		{name: "embedded file", uri: "red:///data/db.rdb",
			errCode: CodeEmbeddedUnsupported},
		{name: "missing host bare scheme", uri: "red://",
			errCode: CodeEmbeddedUnsupported},
	}

	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			got, err := ParseURI(tc.uri)
			if tc.errCode != "" {
				if err == nil {
					t.Fatalf("expected error %s, got nil (parsed=%+v)", tc.errCode, got)
				}
				if !IsCode(err, tc.errCode) {
					t.Fatalf("expected code %s, got %v", tc.errCode, err)
				}
				return
			}
			if err != nil {
				t.Fatalf("unexpected error: %v", err)
			}
			if got.Kind != tc.w.kind {
				t.Errorf("kind: got %s want %s", got.Kind, tc.w.kind)
			}
			if got.Host != tc.w.host {
				t.Errorf("host: got %q want %q", got.Host, tc.w.host)
			}
			if got.Port != tc.w.port {
				t.Errorf("port: got %d want %d", got.Port, tc.w.port)
			}
			if got.Path != tc.w.path {
				t.Errorf("path: got %q want %q", got.Path, tc.w.path)
			}
			if got.Username != tc.w.username {
				t.Errorf("user: got %q want %q", got.Username, tc.w.username)
			}
			if got.Password != tc.w.password {
				t.Errorf("pass: got %q want %q", got.Password, tc.w.password)
			}
			if got.Token != tc.w.token {
				t.Errorf("token: got %q want %q", got.Token, tc.w.token)
			}
			if got.APIKey != tc.w.apiKey {
				t.Errorf("apiKey: got %q want %q", got.APIKey, tc.w.apiKey)
			}
		})
	}
}

func TestHTTPBaseURL(t *testing.T) {
	cases := []struct {
		uri  string
		want string
	}{
		{"http://h:8080", "http://h:8080"},
		{"https://h:8443", "https://h:8443"},
		{"red://h:5050", ""},
	}
	for _, tc := range cases {
		p, err := ParseURI(tc.uri)
		if err != nil {
			t.Fatalf("parse %q: %v", tc.uri, err)
		}
		if got := p.HTTPBaseURL(); got != tc.want {
			t.Errorf("HTTPBaseURL(%q) = %q, want %q", tc.uri, got, tc.want)
		}
	}
}

func TestDeriveLoginURL(t *testing.T) {
	p, err := ParseURI("https://h:9000")
	if err != nil {
		t.Fatal(err)
	}
	got, err := p.DeriveLoginURL()
	if err != nil {
		t.Fatal(err)
	}
	if got != "https://h:9000/auth/login" {
		t.Errorf("got %q", got)
	}

	p, err = ParseURI("red://h?loginUrl=https://other/auth/login")
	if err != nil {
		t.Fatal(err)
	}
	got, err = p.DeriveLoginURL()
	if err != nil {
		t.Fatal(err)
	}
	if got != "https://other/auth/login" {
		t.Errorf("got %q", got)
	}
}
