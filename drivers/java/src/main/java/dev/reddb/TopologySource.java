package dev.reddb;
import com.fasterxml.jackson.databind.node.ObjectNode;

/** Internal transport contract; topology bytes are owned by reddb-wire. */
public interface TopologySource {
    ObjectNode topology(String protocol);
}
