"""Replay the shared RedWire SCRAM-SHA-256 exchange fixtures."""

import base64
import hashlib
import hmac
import json
from pathlib import Path


FIXTURE = (
    Path(__file__).resolve().parents[3]
    / "testdata"
    / "conformance"
    / "redwire"
    / "scram"
    / "exchanges.json"
)
MIN_ITERATIONS = 4096


def _message(vector, direction, kind):
    return next(
        message
        for message in vector["messages"]
        if message["direction"] == direction and message["kind"] == kind
    )


def _parse_server_first(payload, client_nonce):
    fields = {}
    for item in payload.split(","):
        key, separator, value = item.partition("=")
        if not separator or key in fields:
            raise ValueError("malformed SCRAM server-first message")
        fields[key] = value

    combined_nonce = fields.get("r", "")
    if not combined_nonce.startswith(client_nonce) or combined_nonce == client_nonce:
        raise ValueError("server nonce does not extend client nonce")

    try:
        salt = base64.b64decode(fields.get("s", ""), validate=True)
    except ValueError as error:
        raise ValueError("server salt is not valid base64") from error
    if not salt:
        raise ValueError("server salt is empty")

    try:
        iterations = int(fields.get("i", ""))
    except ValueError as error:
        raise ValueError("SCRAM iteration count is not an integer") from error
    if iterations < MIN_ITERATIONS:
        raise ValueError(
            f"SCRAM iteration count {iterations} is below minimum {MIN_ITERATIONS}"
        )
    return combined_nonce, salt, iterations


def _derive_exchange(vector, server_first):
    username = vector["credentials"]["username"]
    password = vector["credentials"]["password"]
    client_nonce = vector["client_nonce"]
    client_first_bare = f"n={username},r={client_nonce}"
    assert _message(vector, "client_to_server", "AuthResponse(client-first)")[
        "payload"
    ] == f"n,,{client_first_bare}"

    combined_nonce, salt, iterations = _parse_server_first(server_first, client_nonce)
    client_final_without_proof = f"c=biws,r={combined_nonce}"
    auth_message = (
        f"{client_first_bare},{server_first},{client_final_without_proof}".encode()
    )
    salted_password = hashlib.pbkdf2_hmac(
        "sha256", password.encode(), salt, iterations, dklen=32
    )
    client_key = hmac.new(salted_password, b"Client Key", hashlib.sha256).digest()
    stored_key = hashlib.sha256(client_key).digest()
    client_signature = hmac.new(stored_key, auth_message, hashlib.sha256).digest()
    proof = bytes(left ^ right for left, right in zip(client_key, client_signature))
    client_final = (
        f"{client_final_without_proof},p={base64.b64encode(proof).decode()}"
    )
    assert _message(vector, "client_to_server", "AuthResponse(client-final)")[
        "payload"
    ] == client_final

    server_key = hmac.new(salted_password, b"Server Key", hashlib.sha256).digest()
    return hmac.new(server_key, auth_message, hashlib.sha256).digest()


def test_shared_scram_exchange_vectors():
    fixture = json.loads(FIXTURE.read_text(encoding="utf-8"))
    assert fixture["version"] == 1
    assert fixture["layout"] == "redwire-scram-sha-256-exchanges-v1"

    for vector in fixture["exchanges"]:
        server_first = _message(vector, "server_to_client", "AuthRequest")["payload"]
        expected = vector["expected"]
        try:
            server_signature = _derive_exchange(vector, server_first)
            auth_ok = json.loads(
                _message(vector, "server_to_client", "AuthOk")["payload"]
            )
            presented_signature = base64.b64decode(auth_ok["v"], validate=True)
            if not hmac.compare_digest(server_signature, presented_signature):
                raise ValueError("SCRAM server signature did not verify")
        except ValueError as error:
            assert expected == {"error": str(error)}, vector["name"]
        else:
            assert expected == {"authenticated": True}, vector["name"]


if __name__ == "__main__":
    test_shared_scram_exchange_vectors()
