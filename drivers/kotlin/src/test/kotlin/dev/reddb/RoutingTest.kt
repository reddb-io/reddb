package dev.reddb

import com.fasterxml.jackson.databind.ObjectMapper
import com.fasterxml.jackson.databind.node.ObjectNode
import kotlinx.coroutines.runBlocking
import org.junit.jupiter.api.Test
import org.junit.jupiter.api.Assertions.*
import java.nio.file.Path
import java.nio.file.Files
import java.util.HexFormat

class RoutingTest {
    private val json = ObjectMapper()
    private fun fixtures(): Path {
        var root = Path.of("").toAbsolutePath()
        while (!Files.exists(root.resolve("testdata/conformance/topology/manifest.json"))) root = root.parent
        return root.resolve("testdata/conformance/topology")
    }
    @Test fun sharedFixtures() {
        for (case in json.readTree(fixtures().resolve("manifest.json").toFile()).path("cases")) {
            val bytes = HexFormat.of().parseHex(case.path("hex").asText())
            if (case.path("error").asBoolean()) assertThrows(RuntimeException::class.java) { TopologyCodec.decode(bytes) }
            else assertEquals(if (case.get("expected").isNull) null else case.get("expected"), TopologyCodec.decode(bytes)?.let { json.readTree(it.toString()) }, case.path("name").asText())
        }
        for (case in json.readTree(fixtures().resolve("sql-policy.json").toFile()))
            assertEquals(case.path("replica_read").asBoolean(), RoutingSql.policy(case.path("sql").asText()).read(), case.path("sql").asText())
    }
    private class Peer(val address: String, val calls: MutableList<Pair<String,String>>, val membership: ObjectNode): Conn, TopologySource {
        override suspend fun topology(protocol:String) = membership
        override suspend fun query(sql:String):ByteArray = query(sql,*emptyArray<Any?>())
        override suspend fun query(sql:String,vararg params:Any?):ByteArray {
            calls += address to sql
            if (sql.contains("lost") || sql.contains("effectful")) throw java.io.IOException("lost reply")
            return address.toByteArray()
        }
        override suspend fun insert(collection:String,payload:Any) {}
        override suspend fun bulkInsert(collection:String,rows:List<Any?>) {}
        override suspend fun get(collection:String,id:String) = query("get")
        override suspend fun delete(collection:String,id:String) {}
        override suspend fun ping() {}
        override fun close() {}
    }
    @Test fun routesAndPinsWithoutReplayingEffects() = runBlocking {
        val primary="https://primary:5000"; val replica="https://replica:5000"
        val calls=mutableListOf<Pair<String,String>>()
        val membership=json.createObjectNode().put("version",1)
        membership.putObject("primary").put("addr",primary)
        membership.putArray("replicas").addObject().put("addr",replica).put("healthy",true).put("lag_ms",10)
        val dial:suspend (String,Boolean)->Conn = {address,_->Peer(address,calls,membership)}
        val client=RoutedConn(primary,dial(primary,false),"https",false,dial)
        assertEquals(primary,String(client.query("SELECT 1")))
        assertEquals(replica,String(client.queryWithOptions("SELECT 1",ReadOptions(ReadConsistency.LOCAL))))
        client.query("BEGIN")
        assertEquals(primary,String(client.queryWithOptions("SELECT 1",ReadOptions(ReadConsistency.LOCAL))))
        client.query("ROLLBACK")
        for (sql in listOf("UPDATE lost","SELECT \"effectful\"()")) {
            val before=calls.size
            try { client.queryWithOptions(sql,ReadOptions(ReadConsistency.LOCAL)); fail<Unit>("expected failure") }
            catch (error:java.io.IOException) { assertEquals(before+1,calls.size);assertEquals(primary,calls.last().first) }
        }
        client.close()
    }
}
