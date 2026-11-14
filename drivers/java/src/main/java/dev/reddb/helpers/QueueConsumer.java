package dev.reddb.helpers;

import java.time.Duration;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ThreadLocalRandom;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

/** At-least-once queue worker with bounded concurrency and renewable leases. */
public final class QueueConsumer implements AutoCloseable {
    @FunctionalInterface
    public interface Handler { void handle(QueueClient.QueueMessage message) throws Exception; }

    @FunctionalInterface
    public interface ErrorHandler {
        void handle(Exception error, QueueClient.QueueMessage message) throws Exception;
    }

    public static final class Options {
        public String consumer;
        public String group;
        public Duration wait = Duration.ofSeconds(30);
        public String retryDelay;
        public int concurrency = 1;
        public Duration receiveBackoffInitial = Duration.ofMillis(250);
        public Duration receiveBackoffMax = Duration.ofSeconds(30);
        public double receiveBackoffMultiplier = 2;
        public boolean receiveBackoffJitter = true;
        public Duration leaseHeartbeat;
        public Duration handlerTimeout;
        public Handler handler;
        public ErrorHandler onError;

        public Options consumer(String value) { consumer = value; return this; }
        public Options group(String value) { group = value; return this; }
        public Options wait(Duration value) { wait = value; return this; }
        public Options retryDelay(String value) { retryDelay = value; return this; }
        public Options concurrency(int value) { concurrency = value; return this; }
        public Options receiveBackoff(Duration initial, Duration max, double multiplier,
                                      boolean jitter) {
            receiveBackoffInitial = initial;
            receiveBackoffMax = max;
            receiveBackoffMultiplier = multiplier;
            receiveBackoffJitter = jitter;
            return this;
        }
        public Options leaseHeartbeat(Duration value) { leaseHeartbeat = value; return this; }
        public Options handlerTimeout(Duration value) { handlerTimeout = value; return this; }
        public Options handler(Handler value) { handler = value; return this; }
        public Options onError(ErrorHandler value) { onError = value; return this; }
    }

    public record Status(String state, boolean running, boolean paused, int inFlight,
                         int concurrency) {}

    private final ScopedQueueClient queue;
    private final Options options;
    private final Object stateLock = new Object();
    private final List<Thread> threads = new ArrayList<>();
    private final AtomicReference<Exception> failure = new AtomicReference<>();
    private final AtomicInteger inFlight = new AtomicInteger();
    private final ScheduledExecutorService scheduler;
    private volatile boolean stopping;
    private volatile boolean aborting;
    private volatile boolean paused;
    private volatile String state = "idle";

    QueueConsumer(ScopedQueueClient queue, Options options) {
        validate(options);
        this.queue = queue;
        this.options = options;
        this.scheduler = Executors.newScheduledThreadPool(Math.max(1, options.concurrency));
    }

    private static void validate(Options options) {
        if (options == null || options.consumer == null || options.consumer.isEmpty()) {
            throw new HelperException.InvalidArgument("queue consumer name must be non-empty");
        }
        Sql.assertIdentifier(options.consumer, "consumer name");
        if (options.group != null && !options.group.isEmpty()) {
            Sql.assertIdentifier(options.group, "group name");
        }
        if (options.wait == null || options.wait.isNegative()) {
            throw new HelperException.InvalidArgument("queue consumer wait must be non-negative");
        }
        if (options.concurrency < 1 || options.concurrency > 128) {
            throw new HelperException.InvalidArgument(
                "queue consumer concurrency must be between 1 and 128");
        }
        if (options.receiveBackoffInitial == null || options.receiveBackoffInitial.isNegative()
            || options.receiveBackoffMax == null
            || options.receiveBackoffMax.compareTo(options.receiveBackoffInitial) < 0
            || options.receiveBackoffMultiplier < 1) {
            throw new HelperException.InvalidArgument("invalid queue receive backoff");
        }
        if ((options.leaseHeartbeat != null && options.leaseHeartbeat.isNegative())
            || (options.handlerTimeout != null && options.handlerTimeout.isNegative())) {
            throw new HelperException.InvalidArgument(
                "queue consumer heartbeat and timeout must be non-negative");
        }
        if (options.handler == null) {
            throw new HelperException.InvalidArgument("queue consumer requires a handler");
        }
    }

    public synchronized QueueConsumer start() {
        if (!threads.isEmpty()) {
            throw new HelperException.InvalidArgument("queue consumer already started");
        }
        state = "running";
        for (int lane = 0; lane < options.concurrency; lane++) {
            Thread thread = new Thread(this::runLane,
                "reddb-queue-" + queue.name() + "-" + lane);
            thread.setDaemon(true);
            threads.add(thread);
            thread.start();
        }
        return this;
    }

    public void stop() {
        synchronized (stateLock) {
            stopping = true;
            paused = false;
            state = "stopping";
            stateLock.notifyAll();
        }
    }

    public void pause() {
        synchronized (stateLock) {
            if (!state.equals("running")) return;
            paused = true;
            state = "paused";
        }
    }

    public void resume() {
        synchronized (stateLock) {
            if (!paused) return;
            paused = false;
            if (state.equals("paused")) state = "running";
            stateLock.notifyAll();
        }
    }

    public void abort() {
        aborting = true;
        stop();
        synchronized (this) {
            for (Thread thread : threads) thread.interrupt();
        }
        scheduler.shutdownNow();
    }

    public void await() throws Exception {
        List<Thread> snapshot;
        synchronized (this) {
            if (threads.isEmpty()) {
                throw new HelperException.InvalidArgument("queue consumer has not started");
            }
            snapshot = List.copyOf(threads);
        }
        for (Thread thread : snapshot) thread.join();
        scheduler.shutdownNow();
        Exception error = failure.get();
        if (error != null) throw error;
    }

    public Status status() {
        return new Status(state, state.equals("running") || state.equals("paused"), paused,
            inFlight.get(), options.concurrency);
    }

    private void runLane() {
        QueueClient.ReadWaitOptions readOptions = new QueueClient.ReadWaitOptions()
            .group(options.group).count(1);
        int receiveFailures = 0;
        try {
            while (waitUntilReady()) {
                List<QueueClient.QueueMessage> messages;
                try {
                    messages = queue.readWait(options.consumer, options.wait, readOptions);
                    receiveFailures = 0;
                } catch (Exception error) {
                    if (aborting) return;
                    report(error, null);
                    receiveFailures++;
                    if (!backoff(receiveFailures)) return;
                    continue;
                }
                if (messages.isEmpty()) continue;
                process(messages.get(0));
            }
        } catch (InterruptedException interrupted) {
            if (!aborting && failure.get() == null) fail(interrupted);
            Thread.currentThread().interrupt();
        } catch (Exception error) {
            fail(error);
        } finally {
            boolean allDone;
            synchronized (this) {
                allDone = threads.stream().allMatch(t -> t == Thread.currentThread()
                    || !t.isAlive());
            }
            if (allDone && failure.get() == null) state = "stopped";
        }
    }

    private void process(QueueClient.QueueMessage message) throws Exception {
        if (message.deliveryId().isEmpty()) {
            report(new HelperException.InvalidResponse(
                "queue delivery is missing delivery_id"), message);
            return;
        }
        inFlight.incrementAndGet();
        Thread lane = Thread.currentThread();
        AtomicReference<Exception> leaseError = new AtomicReference<>();
        AtomicBoolean timedOut = new AtomicBoolean();
        AtomicBoolean handlerDone = new AtomicBoolean();
        Object leaseGate = new Object();
        ScheduledFuture<?> heartbeat = null;
        ScheduledFuture<?> timeout = null;
        if (positive(options.leaseHeartbeat)) {
            long interval = Math.max(1, options.leaseHeartbeat.toMillis());
            heartbeat = scheduler.scheduleAtFixedRate(() -> {
                synchronized (leaseGate) {
                    if (handlerDone.get()) return;
                    try {
                        queue.extend(message.deliveryId());
                    } catch (Exception error) {
                        leaseError.compareAndSet(null, error);
                        lane.interrupt();
                    }
                }
            }, interval, interval, TimeUnit.MILLISECONDS);
        }
        if (positive(options.handlerTimeout)) {
            timeout = scheduler.schedule(() -> {
                if (!handlerDone.get()) {
                    timedOut.set(true);
                    lane.interrupt();
                }
            }, Math.max(1, options.handlerTimeout.toMillis()), TimeUnit.MILLISECONDS);
        }

        Exception handlerError = null;
        try {
            options.handler.handle(message);
        } catch (Exception error) {
            handlerError = error;
        } finally {
            handlerDone.set(true);
            if (heartbeat != null) heartbeat.cancel(false);
            if (timeout != null) timeout.cancel(false);
            synchronized (leaseGate) { /* wait for an in-flight heartbeat */ }
            inFlight.decrementAndGet();
        }
        Thread.interrupted();
        if (aborting) return;
        if (leaseError.get() != null) {
            report(leaseError.get(), message);
            return;
        }
        if (timedOut.get()) {
            queue.nack(message.deliveryId(), options.retryDelay);
            report(new HelperException.QueueHandlerTimeout(
                "queue handler exceeded " + options.handlerTimeout), message);
            return;
        }
        if (handlerError != null) {
            queue.nack(message.deliveryId(), options.retryDelay);
            report(handlerError, message);
            return;
        }
        try {
            queue.ack(message.deliveryId());
        } catch (Exception error) {
            report(error, message);
        }
    }

    private boolean waitUntilReady() throws InterruptedException {
        synchronized (stateLock) {
            while (paused && !stopping) stateLock.wait();
            return !stopping && !aborting;
        }
    }

    private boolean backoff(int failures) throws InterruptedException {
        double millis = options.receiveBackoffInitial.toMillis()
            * Math.pow(options.receiveBackoffMultiplier, failures - 1);
        millis = Math.min(millis, options.receiveBackoffMax.toMillis());
        if (options.receiveBackoffJitter) {
            millis *= ThreadLocalRandom.current().nextDouble();
        }
        synchronized (stateLock) {
            if (stopping) return false;
            stateLock.wait(Math.max(1, (long) millis));
            return !stopping && !aborting;
        }
    }

    private static boolean positive(Duration duration) {
        return duration != null && !duration.isZero() && !duration.isNegative();
    }

    private void fail(Exception error) {
        if (!failure.compareAndSet(null, error)) return;
        aborting = true;
        stop();
        synchronized (this) {
            for (Thread thread : threads) {
                if (thread != Thread.currentThread()) thread.interrupt();
            }
        }
    }

    private void report(Exception error, QueueClient.QueueMessage message) throws Exception {
        if (options.onError == null) throw error;
        options.onError.handle(error, message);
    }

    @Override public void close() { abort(); }
}
