using System;
using System.IO;
using System.Text;
using System.Threading.Tasks;
using Reddb;
using Xunit;

namespace Reddb.Tests;

public class EmbeddedTests
{
    [Fact]
    public async Task MemoryAndSingleFileRoundTripThroughNativeAbi()
    {
        if (Environment.GetEnvironmentVariable("REDDB_EMBEDDED_SMOKE") != "1") return;

        await using (IConn memory = await Reddb.ConnectAsync("memory://"))
        {
            await memory.QueryAsync("CREATE TABLE dotnet_embedded (id INTEGER, name TEXT)");
            await memory.QueryAsync(
                "INSERT INTO dotnet_embedded (id, name) VALUES ($1, $2)",
                7,
                "embedded");
            ReadOnlyMemory<byte> rows = await memory.QueryAsync(
                "SELECT name FROM dotnet_embedded WHERE id = $1",
                7);
            Assert.Contains("embedded", Encoding.UTF8.GetString(rows.Span));
        }

        string directory = Directory.CreateTempSubdirectory("reddb-dotnet-embedded-").FullName;
        string database = Path.Combine(directory, "driver-contract.rdb");
        string uri = new Uri(database).AbsoluteUri;
        try
        {
            await using (IConn first = await Reddb.ConnectAsync(uri))
            {
                await first.QueryAsync("CREATE TABLE durable (id INTEGER, name TEXT)");
                await first.QueryAsync("INSERT INTO durable (id, name) VALUES (1, 'persisted')");
                await Assert.ThrowsAnyAsync<Exception>(async () =>
                {
                    await using IConn _ = await Reddb.ConnectAsync(uri);
                });
            }

            await using (IConn second = await Reddb.ConnectAsync(uri))
            {
                ReadOnlyMemory<byte> rows = await second.QueryAsync(
                    "SELECT name FROM durable WHERE id = 1");
                Assert.Contains("persisted", Encoding.UTF8.GetString(rows.Span));
            }

            Assert.Equal(new[] { database }, Directory.GetFiles(directory));
        }
        finally
        {
            Directory.Delete(directory, recursive: true);
        }
    }
}
