"""URL parser tests — mirror drivers/js/src/url.js semantics."""

from __future__ import annotations

import pytest

from reddb_asyncio.errors import InvalidUri, UnsupportedScheme
from reddb_asyncio.url import default_port_for, parse_uri


def test_red_default_port():
    p = parse_uri("red://localhost")
    assert p.kind == "redwire"
    assert p.host == "localhost"
    assert p.port == 5050


def test_red_explicit_port():
    p = parse_uri("red://10.0.0.5:7777")
    assert p.kind == "redwire"
    assert p.host == "10.0.0.5"
    assert p.port == 7777


def test_reds_tls_default_port():
    p = parse_uri("reds://example.com")
    assert p.kind == "redwire-tls"
    assert p.port == 5050


def test_http_default_port():
    p = parse_uri("http://api.example.com")
    assert p.kind == "http"
    assert p.port == 8080


def test_https_default_port():
    p = parse_uri("https://api.example.com")
    assert p.kind == "https"
    assert p.port == 8443


def test_user_pass_decoding():
    p = parse_uri("red://alice:p%40ssw0rd@host:5050")
    assert p.username == "alice"
    assert p.password == "p@ssw0rd"


def test_query_token():
    p = parse_uri("red://host?token=sk-abc-123")
    assert p.token == "sk-abc-123"


def test_query_auth_choice():
    p = parse_uri("red://alice:secret@host?auth=scram")
    assert p.auth == "scram"


def test_invalid_auth_choice_raises():
    with pytest.raises(InvalidUri):
        parse_uri("red://host?auth=garbage")


def test_query_sslmode_promotes_to_tls():
    p = parse_uri("red://host?sslmode=require")
    assert p.kind == "redwire-tls"


def test_query_timeout_ms_int():
    p = parse_uri("red://host?timeout_ms=12500")
    assert p.timeout_ms == 12500


def test_query_timeout_ms_invalid():
    with pytest.raises(InvalidUri):
        parse_uri("red://host?timeout_ms=fast")


def test_tls_files_in_query():
    p = parse_uri("reds://host?ca=/etc/ca.pem&cert=/etc/c.pem&key=/etc/k.pem")
    assert p.ca == "/etc/ca.pem"
    assert p.cert == "/etc/c.pem"
    assert p.key == "/etc/k.pem"


def test_proto_override_to_https():
    p = parse_uri("red://host:9000?proto=https")
    assert p.kind == "https"
    assert p.port == 9000


def test_proto_override_to_reds():
    p = parse_uri("red://host?proto=reds")
    assert p.kind == "redwire-tls"


def test_proto_override_grpc_aliases_redwire():
    p = parse_uri("red://host?proto=grpc")
    assert p.kind == "redwire"


def test_embedded_in_memory_red_only():
    for uri in ("red://", "red:", "red://memory", "red://memory/", "red://:memory", "red://:memory:"):
        p = parse_uri(uri)
        assert p.kind == "embedded", uri
        assert p.path is None


def test_embedded_with_absolute_path():
    p = parse_uri("red:///var/lib/reddb/data.rdb")
    assert p.kind == "embedded"
    assert p.path == "/var/lib/reddb/data.rdb"


def test_unsupported_scheme():
    with pytest.raises(UnsupportedScheme):
        parse_uri("mongodb://host")


def test_empty_uri():
    with pytest.raises(InvalidUri):
        parse_uri("")


def test_default_port_for_aliases():
    assert default_port_for("redwire") == 5050
    assert default_port_for("http") == 8080
    assert default_port_for("https") == 8443
    assert default_port_for("nonsense") == 5050


def test_token_via_url_keeps_user_pass():
    p = parse_uri("red://u:pw@host?token=tok")
    assert p.username == "u"
    assert p.password == "pw"
    assert p.token == "tok"


def test_original_uri_preserved():
    uri = "red://host?proto=https&token=x"
    p = parse_uri(uri)
    assert p.original_uri == uri


def test_proto_override_unknown():
    with pytest.raises(UnsupportedScheme):
        parse_uri("red://host?proto=ftp")
