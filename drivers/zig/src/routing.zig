const std = @import("std");
const root = @import("reddb.zig");
const wire = @import("topology.zig");
const Allocator = std.mem.Allocator;

pub const Consistency = enum { strong, local, bounded_staleness };
pub const ReadOptions = struct { consistency: Consistency = .strong, lag_ms_max: u32 = 0 };

pub const Router = struct {
    allocator: Allocator,
    configuration: std.heap.ArenaAllocator,
    seed_uri: []const u8 = "",
    seed: []const u8 = "",
    primary: []const u8 = "",
    protocol: []const u8,
    override: ?root.redwire.ConnectOptions = null,
    force_primary: bool = false,
    clients: std.StringHashMap(*root.Conn),
    membership: ?wire.Topology = null,
    pinned: ?[]const u8 = null,
    session: bool = false,
    broken: bool = false,
    closed: bool = false,
    next: usize = 0,
    refresh_at: i64 = 0,
    mutex: std.Thread.Mutex = .{},

    pub fn create(allocator: Allocator, uri: []const u8, seed: []const u8, protocol: []const u8, initial: *root.Conn, override: ?root.redwire.ConnectOptions) !*Router {
        var transferred = false;
        errdefer if (!transferred) {
            initial.deinit();
            allocator.destroy(initial);
        };
        const self = try allocator.create(Router);
        self.* = .{ .allocator = allocator, .configuration = std.heap.ArenaAllocator.init(allocator), .protocol = protocol, .clients = std.StringHashMap(*root.Conn).init(allocator) };
        errdefer {
            self.deinit();
            allocator.destroy(self);
        }
        const storage = self.configuration.allocator();
        self.seed_uri = try storage.dupe(u8, uri);
        self.seed = try storage.dupe(u8, seed);
        self.primary = self.seed;
        const parsed_uri = try std.Uri.parse(uri);
        if (parsed_uri.query) |query_component| {
            var pairs = std.mem.splitScalar(u8, query_component.percent_encoded, '&');
            while (pairs.next()) |pair| if (std.ascii.eqlIgnoreCase(pair, "route=primary")) {
                self.force_primary = true;
            };
        }
        if (override) |value| {
            var clone = value;
            clone.host = try storage.dupe(u8, value.host);
            clone.client_name = if (value.client_name) |name| try storage.dupe(u8, name) else null;
            clone.auth = switch (value.auth) {
                .anonymous => .{ .anonymous = {} },
                .bearer => |token| .{ .bearer = try storage.dupe(u8, token) },
                .scram_sha_256 => |credentials| .{ .scram_sha_256 = .{ .username = try storage.dupe(u8, credentials.username), .password = try storage.dupe(u8, credentials.password) } },
            };
            self.override = clone;
        }
        const key = try allocator.dupe(u8, seed);
        self.clients.put(key, initial) catch |err| {
            allocator.free(key);
            return err;
        };
        transferred = true;
        try self.refresh();
        return self;
    }
    fn dial(self: *Router, address: []const u8) !*root.Conn {
        var target = try root.url.parse(self.allocator, address);
        defer target.deinit(self.allocator);
        var original = try root.url.parse(self.allocator, self.seed_uri);
        defer original.deinit(self.allocator);
        var options = self.override orelse root.redwire.ConnectOptions{
            .host = target.host orelse return error.MissingHost,
            .port = target.port orelse root.url.defaultPortFor(target.kind),
        };
        options.host = target.host orelse return error.MissingHost;
        options.port = target.port orelse root.url.defaultPortFor(target.kind);
        options.tls = std.mem.eql(u8, self.protocol, "reds");
        options.connect_timeout_ms = @min(options.connect_timeout_ms, 5000);
        options.io_timeout_ms = @min(options.io_timeout_ms, 5000);
        if (self.override == null) {
            if (original.token orelse original.api_key) |token| {
                options.auth = .{ .bearer = token };
            } else if (original.username) |username| {
                if (original.password) |password| options.auth = .{ .scram_sha_256 = .{ .username = username, .password = password } };
            }
        }
        return root.connectDirect(self.allocator, address, options);
    }
    fn endpoint(self: *Router, address: []const u8) !*root.Conn {
        if (self.clients.get(address)) |client| return client;
        const client = try self.dial(address);
        errdefer {
            client.deinit();
            self.allocator.destroy(client);
        }
        try client.ping();
        const key = try self.allocator.dupe(u8, address);
        errdefer self.allocator.free(key);
        try self.clients.put(key, client);
        return client;
    }
    fn remove(self: *Router, address: []const u8) void {
        if (self.clients.fetchRemove(address)) |entry| {
            entry.value.deinit();
            self.allocator.destroy(entry.value);
            self.allocator.free(entry.key);
        }
    }
    pub fn refresh(self: *Router) !void {
        if (self.pinned != null or self.closed) return;
        self.refresh_at = std.time.milliTimestamp() + 30_000;
        const deadline = std.time.milliTimestamp() + 5000;
        var temporary = std.heap.ArenaAllocator.init(self.allocator);
        defer temporary.deinit();
        const storage = temporary.allocator();
        var candidates = std.ArrayList([]const u8).init(storage);
        try candidates.append(try storage.dupe(u8, self.primary));
        try candidates.append(try storage.dupe(u8, self.seed));
        if (self.membership) |membership| for (membership.replicas) |replica| {
            try candidates.append(try storage.dupe(u8, replica.addr));
        };
        var index: usize = 0;
        while (index < candidates.items.len and index < 258 and std.time.milliTimestamp() < deadline) : (index += 1) {
            const candidate = candidates.items[index];
            const control = self.dial(candidate) catch continue;
            defer {
                control.deinit();
                self.allocator.destroy(control);
            }
            var next = (control.topology(self.protocol) catch |err| {
                if (err == error.AuthRefused) return err;
                continue;
            }) orelse continue;
            var published = false;
            defer if (!published) next.deinit();
            const primary = (try wire.address(next.arena.allocator(), next.primary.addr, self.protocol)) orelse continue;
            next.primary.addr = primary;
            var seen = false;
            for (candidates.items) |item| if (std.mem.eql(u8, item, primary)) {
                seen = true;
            };
            if (!seen) try candidates.append(try storage.dupe(u8, primary));
            _ = self.endpoint(primary) catch continue;
            var ready: usize = 0;
            for (next.replicas) |replica| {
                if (std.time.milliTimestamp() >= deadline) break;
                if (!replica.healthy or replica.rebootstrapping) continue;
                const address = (try wire.address(next.arena.allocator(), replica.addr, self.protocol)) orelse continue;
                if (std.mem.eql(u8, address, primary)) continue;
                _ = self.endpoint(address) catch continue;
                next.replicas[ready] = replica;
                next.replicas[ready].addr = address;
                ready += 1;
            }
            next.replicas = next.replicas[0..ready];
            var previous = self.membership;
            self.membership = next;
            published = true;
            self.primary = primary;
            if (previous) |*old| old.deinit();
            var retired = std.ArrayList([]const u8).init(storage);
            var iterator = self.clients.keyIterator();
            while (iterator.next()) |key| {
                var keep = std.mem.eql(u8, key.*, self.seed) or std.mem.eql(u8, key.*, self.primary);
                for (next.replicas) |replica| keep = keep or std.mem.eql(u8, key.*, replica.addr);
                if (!keep) try retired.append(key.*);
            }
            for (retired.items) |address| self.remove(address);
            if (std.mem.eql(u8, candidate, primary)) return;
        }
    }
    fn network(err: anyerror) bool {
        return switch (err) {
            error.ConnectionRefused, error.ConnectionResetByPeer, error.BrokenPipe, error.ConnectionTimedOut, error.SocketNotConnected, error.UnexpectedFrame, error.EndOfStream, error.ConnectionClosed => true,
            else => false,
        };
    }
    pub fn query(self: *Router, sql: []const u8, params: anytype, options: ?ReadOptions) anyerror![]const u8 {
        self.mutex.lock();
        defer self.mutex.unlock();
        if (self.closed or self.broken) return error.ConnectionClosed;
        if (self.pinned == null and std.time.milliTimestamp() >= self.refresh_at) try self.refresh();
        const policy = try wire.sqlPolicy(self.allocator, sql);
        const read = policy.read and options != null;
        var address = self.pinned orelse self.primary;
        if (self.pinned == null and read and !self.force_primary and options.?.consistency != .strong) {
            var eligible = std.ArrayList([]const u8).init(self.allocator);
            defer eligible.deinit();
            if (self.membership) |membership| for (membership.replicas) |replica| {
                if (options.?.consistency != .bounded_staleness or replica.lag_ms <= options.?.lag_ms_max) try eligible.append(replica.addr);
            };
            if (eligible.items.len > 0) {
                address = eligible.items[self.next % eligible.items.len];
                self.next +%= 1;
            }
        }
        if (policy.state == .begin or policy.state == .session) {
            self.pinned = address;
            self.session = self.session or policy.state == .session;
        }
        const client = try self.endpoint(address);
        const result = client.query(sql, params) catch |err| {
            if (!network(err)) return err;
            self.refresh_at = 0;
            self.remove(address);
            if (self.pinned != null) self.broken = true;
            if (self.pinned != null or !read) return err;
            try self.refresh();
            return (try self.endpoint(self.primary)).query(sql, params);
        };
        if (policy.state == .end and !self.session) self.pinned = null;
        return result;
    }
    pub fn ping(self: *Router) anyerror!void {
        self.mutex.lock();
        defer self.mutex.unlock();
        if (self.closed or self.broken) return error.ConnectionClosed;
        if (self.pinned == null and std.time.milliTimestamp() >= self.refresh_at) try self.refresh();
        return (try self.endpoint(self.pinned orelse self.primary)).ping();
    }
    pub fn close(self: *Router) void {
        self.mutex.lock();
        defer self.mutex.unlock();
        self.closed = true;
        var iterator = self.clients.iterator();
        while (iterator.next()) |entry| {
            entry.value_ptr.*.deinit();
            self.allocator.destroy(entry.value_ptr.*);
            self.allocator.free(entry.key_ptr.*);
        }
        self.clients.clearRetainingCapacity();
    }
    pub fn deinit(self: *Router) void {
        self.close();
        self.clients.deinit();
        if (self.membership) |*membership| membership.deinit();
        self.configuration.deinit();
    }
};
