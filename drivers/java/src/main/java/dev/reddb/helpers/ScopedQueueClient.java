package dev.reddb.helpers;

import java.time.Duration;
import java.util.List;

/** Queue helper bound to one explicit queue name. */
public final class ScopedQueueClient {
    private final QueueClient queue;
    private final String name;

    ScopedQueueClient(QueueClient queue, String name) {
        this.queue = queue;
        this.name = name;
    }

    public String name() { return name; }
    public Envelopes.QueuePushResult push(Object payload) { return queue.push(name, payload); }
    public Envelopes.QueuePushResult push(Object payload, QueueClient.PushOptions options) {
        return queue.push(name, payload, options);
    }
    public List<Object> pop() { return queue.pop(name); }
    public List<Object> pop(int count) { return queue.pop(name, count); }
    public List<Object> peek() { return queue.peek(name); }
    public List<Object> peek(int count) { return queue.peek(name, count); }
    public long len() { return queue.len(name); }
    public Envelopes.DeleteResult purge() { return queue.purge(name); }

    public List<QueueClient.QueueMessage> readWait(
        String consumer, Duration wait, QueueClient.ReadWaitOptions options
    ) {
        return queue.readWaitEntries(name, consumer, wait, options);
    }

    public void ack(String deliveryId) { queue.ack(name, deliveryId); }
    public void nack(String deliveryId) { queue.nack(name, deliveryId, null); }
    public void nack(String deliveryId, String delay) { queue.nack(name, deliveryId, delay); }
    public QueueConsumer consumer(QueueConsumer.Options options) {
        return new QueueConsumer(this, options);
    }
}
