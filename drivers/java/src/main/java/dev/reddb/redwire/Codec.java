package dev.reddb.redwire;

import dev.reddb.RedDBException;

import java.lang.reflect.Method;

/**
 * Lazy wrapper around {@code com.github.luben:zstd-jni}. We don't
 * link against zstd-jni at compile time (the dependency is on the
 * implementation classpath but not exported) so reflection lets the
 * driver still boot when the jar is missing — only inbound
 * COMPRESSED frames will throw {@link RedDBException.CompressedButNoZstd}.
 */
final class Codec {
    private static volatile Boolean available;
    private static volatile Method compressMethod;     // Zstd.compress(byte[], int)
    private static volatile Method decompressMethod;   // Zstd.decompress(byte[], int)
    private static volatile Method decompressedSize;   // Zstd.decompressedSize(byte[])
    private static volatile Throwable initFailure;

    private Codec() {}

    private static synchronized void init() {
        if (available != null) return;
        try {
            Class<?> zstd = Class.forName("com.github.luben.zstd.Zstd");
            compressMethod = zstd.getMethod("compress", byte[].class, int.class);
            decompressMethod = zstd.getMethod("decompress", byte[].class, int.class);
            decompressedSize = zstd.getMethod("decompressedSize", byte[].class);
            available = Boolean.TRUE;
        } catch (Throwable t) {
            initFailure = t;
            available = Boolean.FALSE;
        }
    }

    static boolean isAvailable() {
        if (available == null) init();
        return Boolean.TRUE.equals(available);
    }

    /** Compress a payload at level 1 (matches the engine default). */
    static byte[] compress(byte[] plaintext) {
        if (available == null) init();
        if (!Boolean.TRUE.equals(available)) {
            throw new RedDBException.CompressedButNoZstd(
                "zstd-jni not on classpath — cannot encode COMPRESSED frame", initFailure);
        }
        int level = parseLevel();
        try {
            Object result = compressMethod.invoke(null, plaintext, level);
            return (byte[]) result;
        } catch (Throwable t) {
            throw new RedDBException.ProtocolError("zstd compress failed: " + t.getMessage(), t);
        }
    }

    /** Decompress a payload. Throws {@link RedDBException.CompressedButNoZstd} when zstd is absent. */
    static byte[] decompress(byte[] compressed) {
        if (available == null) init();
        if (!Boolean.TRUE.equals(available)) {
            throw new RedDBException.CompressedButNoZstd(
                "incoming frame has COMPRESSED flag but zstd-jni isn't loadable", initFailure);
        }
        try {
            long size = (long) decompressedSize.invoke(null, (Object) compressed);
            if (size < 0 || size > Frame.MAX_FRAME_SIZE) {
                throw new RedDBException.ProtocolError(
                    "zstd decompressed size out of range: " + size);
            }
            Object result = decompressMethod.invoke(null, compressed, (int) size);
            return (byte[]) result;
        } catch (RedDBException re) {
            throw re;
        } catch (Throwable t) {
            throw new RedDBException.ProtocolError("zstd decompress failed: " + t.getMessage(), t);
        }
    }

    private static int parseLevel() {
        String env = System.getenv("RED_REDWIRE_ZSTD_LEVEL");
        if (env == null || env.isEmpty()) return 1;
        try {
            int n = Integer.parseInt(env.trim());
            if (n < 1 || n > 22) return 1;
            return n;
        } catch (NumberFormatException e) {
            return 1;
        }
    }
}
