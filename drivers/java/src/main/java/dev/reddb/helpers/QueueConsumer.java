package dev.reddb.helpers;

import java.time.Duration;
import java.util.List;

/** Sequential at-least-once queue worker. Use one dedicated connection per worker. */
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
        public Handler handler;
        public ErrorHandler onError;

        public Options consumer(String value) { consumer = value; return this; }
        public Options group(String value) { group = value; return this; }
        public Options wait(Duration value) { wait = value; return this; }
        public Options retryDelay(String value) { retryDelay = value; return this; }
        public Options handler(Handler value) { handler = value; return this; }
        public Options onError(ErrorHandler value) { onError = value; return this; }
    }

    private final ScopedQueueClient queue;
    private final Options options;
    private volatile boolean stopping;
    private Thread thread;
    private Exception failure;

    QueueConsumer(ScopedQueueClient queue, Options options) {
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
        if (options.handler == null) {
            throw new HelperException.InvalidArgument("queue consumer requires a handler");
        }
        this.queue = queue;
        this.options = options;
    }

    public synchronized QueueConsumer start() {
        if (thread != null) {
            throw new HelperException.InvalidArgument("queue consumer already started");
        }
        thread = new Thread(this::run, "reddb-queue-" + queue.name());
        thread.setDaemon(true);
        thread.start();
        return this;
    }

    public void stop() { stopping = true; }
    public void abort() {
        stopping = true;
        Thread running = thread;
        if (running != null) running.interrupt();
    }

    public void await() throws Exception {
        Thread running = thread;
        if (running == null) {
            throw new HelperException.InvalidArgument("queue consumer has not started");
        }
        running.join();
        if (failure != null) throw failure;
    }

    private void run() {
        QueueClient.ReadWaitOptions readOptions = new QueueClient.ReadWaitOptions()
            .group(options.group).count(1);
        try {
            while (!stopping && !Thread.currentThread().isInterrupted()) {
                List<QueueClient.QueueMessage> messages;
                try {
                    messages = queue.readWait(options.consumer, options.wait, readOptions);
                } catch (Exception error) {
                    report(error, null);
                    continue;
                }
                if (messages.isEmpty()) continue;
                QueueClient.QueueMessage message = messages.get(0);
                try {
                    if (message.deliveryId().isEmpty()) {
                        throw new HelperException.InvalidResponse(
                            "queue delivery is missing delivery_id");
                    }
                    options.handler.handle(message);
                } catch (Exception error) {
                    if (!message.deliveryId().isEmpty()) {
                        queue.nack(message.deliveryId(), options.retryDelay);
                    }
                    report(error, message);
                    continue;
                }
                try {
                    queue.ack(message.deliveryId());
                } catch (Exception error) {
                    report(error, message);
                }
            }
        } catch (InterruptedException interrupted) {
            Thread.currentThread().interrupt();
        } catch (Exception error) {
            failure = error;
        }
    }

    private void report(Exception error, QueueClient.QueueMessage message) throws Exception {
        if (options.onError == null) throw error;
        options.onError.handle(error, message);
    }

    @Override public void close() { abort(); }
}
