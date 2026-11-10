package dev.reddb.embedded;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.sun.jna.Library;
import com.sun.jna.Native;
import com.sun.jna.Pointer;
import com.sun.jna.ptr.PointerByReference;
import dev.reddb.Conn;
import dev.reddb.RedDBException;

import java.nio.charset.StandardCharsets;
import java.util.List;

/** Blocking Java facade over the versioned RedDB embedded C ABI. */
public final class EmbeddedConn implements Conn {
    private static final int ABI_VERSION = 1;
    private static final ObjectMapper MAPPER = new ObjectMapper();
    private static volatile NativeApi loadedApi;

    private interface NativeApi extends Library {
        int reddb_abi_version();
        int reddb_open(String path, PointerByReference handle, PointerByReference error);
        int reddb_invoke(Pointer handle, String method, String params,
                         PointerByReference result, PointerByReference error);
        int reddb_close(Pointer handle, PointerByReference error);
        void reddb_string_free(Pointer value);
    }

    private final NativeApi api;
    private Pointer handle;

    private EmbeddedConn(NativeApi api, Pointer handle) {
        this.api = api;
        this.handle = handle;
    }

    public static EmbeddedConn open(String path) {
        NativeApi api = nativeApi();
        PointerByReference handle = new PointerByReference();
        PointerByReference error = new PointerByReference();
        int status = api.reddb_open(path == null ? "" : path, handle, error);
        String message = takeString(api, error.getValue());
        if (status != 0) {
            throw new RedDBException.EngineError(message.isEmpty()
                ? "reddb_open failed with status " + status : message);
        }
        if (handle.getValue() == null) {
            throw new RedDBException.ProtocolError("reddb_open returned a null handle");
        }
        return new EmbeddedConn(api, handle.getValue());
    }

    private static synchronized NativeApi nativeApi() {
        if (loadedApi != null) return loadedApi;
        String path = System.getenv("REDDB_FFI_LIBRARY");
        if (path == null || path.isBlank()) path = "reddb_ffi";
        try {
            NativeApi api = Native.load(path, NativeApi.class);
            int version = api.reddb_abi_version();
            if (version != ABI_VERSION) {
                throw new RedDBException.ProtocolError(
                    "incompatible RedDB embedded ABI: expected " + ABI_VERSION + ", found " + version);
            }
            loadedApi = api;
            return api;
        } catch (RedDBException e) {
            throw e;
        } catch (Throwable e) {
            throw new RedDBException.ProtocolError("embedded native library unavailable: " + e.getMessage(), e);
        }
    }

    private static String takeString(NativeApi api, Pointer pointer) {
        if (pointer == null) return "";
        try {
            return pointer.getString(0, StandardCharsets.UTF_8.name());
        } finally {
            api.reddb_string_free(pointer);
        }
    }

    private synchronized byte[] invoke(String method, ObjectNode params) {
        if (handle == null) throw new IllegalStateException("embedded connection is closed");
        try {
            String encoded = MAPPER.writeValueAsString(params);
            PointerByReference result = new PointerByReference();
            PointerByReference error = new PointerByReference();
            int status = api.reddb_invoke(handle, method, encoded, result, error);
            String message = takeString(api, error.getValue());
            if (status != 0) {
                Pointer resultPointer = result.getValue();
                if (resultPointer != null) api.reddb_string_free(resultPointer);
                throw new RedDBException.EngineError(message.isEmpty()
                    ? "reddb_invoke failed with status " + status : message);
            }
            if (result.getValue() == null) {
                throw new RedDBException.ProtocolError("reddb_invoke returned a null result");
            }
            return takeString(api, result.getValue()).getBytes(StandardCharsets.UTF_8);
        } catch (RedDBException e) {
            throw e;
        } catch (Exception e) {
            throw new RedDBException.ProtocolError("embedded call failed: " + e.getMessage(), e);
        }
    }

    @Override public byte[] query(String sql) { return query(sql, new Object[0]); }

    @Override public byte[] query(String sql, Object... params) {
        ObjectNode body = MAPPER.createObjectNode().put("sql", sql);
        if (params != null && params.length > 0) body.set("params", MAPPER.valueToTree(params));
        return invoke("query", body);
    }

    @Override public void insert(String collection, Object payload) {
        ObjectNode body = MAPPER.createObjectNode().put("collection", collection);
        body.set("payload", MAPPER.valueToTree(payload));
        invoke("insert", body);
    }

    @Override public void bulkInsert(String collection, List<?> rows) {
        ObjectNode body = MAPPER.createObjectNode().put("collection", collection);
        body.set("payloads", MAPPER.valueToTree(rows));
        invoke("bulk_insert", body);
    }

    @Override public byte[] get(String collection, String id) {
        return invoke("get", MAPPER.createObjectNode().put("collection", collection).put("id", id));
    }

    @Override public void delete(String collection, String id) {
        invoke("delete", MAPPER.createObjectNode().put("collection", collection).put("id", id));
    }

    @Override public void ping() { invoke("health", MAPPER.createObjectNode()); }

    @Override public synchronized void close() {
        if (handle == null) return;
        Pointer current = handle;
        handle = null;
        PointerByReference error = new PointerByReference();
        int status = api.reddb_close(current, error);
        String message = takeString(api, error.getValue());
        if (status != 0) {
            throw new RedDBException.EngineError(message.isEmpty()
                ? "reddb_close failed with status " + status : message);
        }
    }
}
