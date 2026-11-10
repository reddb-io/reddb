package dev.reddb;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ArrayNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import java.net.URI;
import java.nio.ByteBuffer;
import java.nio.ByteOrder;
import java.nio.charset.StandardCharsets;
import java.nio.charset.CodingErrorAction;

/** Translation of reddb-wire::topology, pinned by shared conformance fixtures. */
public final class TopologyCodec {
    private TopologyCodec() {}
    public static final int KIND = 0x32;
    public static final int FEATURE = 2;
    private static final ObjectMapper JSON = new ObjectMapper();

    public static ObjectNode decode(byte[] bytes) {
        try {
            if (bytes.length < 5) throw new IllegalArgumentException("truncated topology");
            ByteBuffer input = ByteBuffer.wrap(bytes).order(ByteOrder.LITTLE_ENDIAN);
            int version = input.get() & 255;
            long size = Integer.toUnsignedLong(input.getInt());
            if (size > input.remaining()) throw new IllegalArgumentException("truncated topology body");
            if (version != 1) return null;
            input.limit(5 + (int)size);
            ObjectNode topology = JSON.createObjectNode().put("version", 1)
                .put("epoch", Long.toUnsignedString(input.getLong()));
            topology.set("primary", JSON.createObjectNode().put("addr", text(input)).put("region", text(input)));
            long count = Integer.toUnsignedLong(input.getInt());
            if (count > 256) throw new IllegalArgumentException("too many topology replicas");
            ArrayNode replicas = topology.putArray("replicas");
            for (int index = 0; index < count; index++)
                replicas.add(JSON.createObjectNode().put("addr", text(input)).put("region", text(input))
                    .put("healthy", input.get() != 0).put("lag_ms", Integer.toUnsignedLong(input.getInt()))
                    .put("last_applied_lsn", Long.toUnsignedString(input.getLong())).put("rebootstrapping", false));
            if (input.remaining() >= count)
                for (var replica : replicas) ((ObjectNode)replica).put("rebootstrapping", input.get() != 0);
            return topology;
        } catch (Exception error) {
            throw new RedDBException.ProtocolError("invalid topology payload", error);
        }
    }
    private static String text(ByteBuffer input) throws Exception {
        long size = Integer.toUnsignedLong(input.getInt());
        if (size > 4096 || size > input.remaining()) throw new IllegalArgumentException("invalid topology string");
        ByteBuffer slice = input.slice(); slice.limit((int)size); input.position(input.position() + (int)size);
        return StandardCharsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT).decode(slice).toString();
    }
    public static String address(String address, String protocol) {
        if (address == null || address.isEmpty() || address.length() > 4096) return null;
        try {
            URI uri = URI.create(address.contains("://") ? address : protocol + "://" + address);
            if (!protocol.equals(uri.getScheme()) || uri.getHost() == null || uri.getUserInfo() != null
                || uri.getRawQuery() != null || uri.getRawFragment() != null) return null;
            return uri.toASCIIString().replaceAll("/+$", "");
        } catch (IllegalArgumentException error) { return null; }
    }
}
