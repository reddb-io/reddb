from reddb_asyncio.client import _kv_path


def test_kv_path_quotes_namespaced_keys_without_rewriting():
    assert _kv_path("kv_default", "corpus:version") == "kv_default.'corpus:version'"
