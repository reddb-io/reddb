package dev.reddb;

import java.time.Duration;

/**
 * Immutable bag of optional connection knobs. Pass to
 * {@link Reddb#connect(java.net.URI, Options)}. Use the static
 * builder when you need to override a default.
 */
public final class Options {
    /** Default — empty, no overrides. */
    public static final Options DEFAULTS = new Options(null, null, null, null, null, Duration.ofSeconds(30));

    private final String username;
    private final String password;
    private final String token;
    private final String apiKey;
    private final String clientName;
    private final Duration timeout;

    private Options(String username, String password, String token, String apiKey,
                    String clientName, Duration timeout) {
        this.username = username;
        this.password = password;
        this.token = token;
        this.apiKey = apiKey;
        this.clientName = clientName;
        this.timeout = timeout == null ? Duration.ofSeconds(30) : timeout;
    }

    public String username() { return username; }
    public String password() { return password; }
    public String token() { return token; }
    public String apiKey() { return apiKey; }
    public String clientName() { return clientName; }
    public Duration timeout() { return timeout; }

    public static Builder builder() { return new Builder(); }

    public static final class Builder {
        private String username;
        private String password;
        private String token;
        private String apiKey;
        private String clientName;
        private Duration timeout = Duration.ofSeconds(30);

        public Builder username(String v) { this.username = v; return this; }
        public Builder password(String v) { this.password = v; return this; }
        public Builder token(String v) { this.token = v; return this; }
        public Builder apiKey(String v) { this.apiKey = v; return this; }
        public Builder clientName(String v) { this.clientName = v; return this; }
        public Builder timeout(Duration v) { this.timeout = v; return this; }

        public Options build() {
            return new Options(username, password, token, apiKey, clientName, timeout);
        }
    }
}
