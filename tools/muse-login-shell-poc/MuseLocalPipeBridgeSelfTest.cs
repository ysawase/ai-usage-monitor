using System.IO.Pipes;
using System.Text;
using System.Text.Json;

namespace MuseLoginShellPoc;

// Synthetic only. No Muse requests, cookies or account sign-in.
internal static class MuseLocalPipeBridgeSelfTest
{
    internal static async Task<bool> RunAsync()
    {
        string name = MuseLocalPipeBridge.DefaultName + "Synthetic"
            + Guid.NewGuid().ToString("N");
        int count = 0;
        using var bridge = new MuseLocalPipeBridge(_ =>
        {
            count++;
            return Task.FromResult(new MuseAutomaticResult(
                MuseAutomaticStatus.Success,
                new MusePassiveQuotaReading(
                    "Muse Free", 7M,
                    DateTimeOffset.FromUnixTimeSeconds(1794584040L))));
        }, name);
        bridge.Start();
        var one = await ReadAsync(name, "READ\n");
        var two = await ReadAsync(name, "READ\n");
        var invalid = await ReadAsync(name, "NOPE\n");
        bridge.Stop();
        return count == 2
            && Check(one, "success", 7M, 1794584040L)
            && Check(two, "success", 7M, 1794584040L)
            && Check(invalid, "unavailable", null, null)
            && !one.Contains("Muse Free", StringComparison.Ordinal)
            && !one.Contains("Cookie", StringComparison.OrdinalIgnoreCase);
    }

    private static async Task<string> ReadAsync(string name, string command)
    {
        using var pipe = new NamedPipeClientStream(".", name, PipeDirection.InOut,
            PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly);
        using var ct = new CancellationTokenSource(TimeSpan.FromSeconds(8));
        await pipe.ConnectAsync(ct.Token);
        await pipe.WriteAsync(Encoding.ASCII.GetBytes(command), ct.Token);
        await pipe.FlushAsync(ct.Token);
        var output = new byte[512];
        int n = await pipe.ReadAsync(output.AsMemory(), ct.Token);
        return Encoding.UTF8.GetString(output, 0, n).TrimEnd('\n');
    }

    private static bool Check(string raw, string status, decimal? used, long? reset)
    {
        try
        {
            using var doc = JsonDocument.Parse(raw);
            var root = doc.RootElement;
            var value = root.GetProperty("used_percent");
            var time = root.GetProperty("resets_at_epoch_seconds");
            return root.GetProperty("schema_version").GetInt32() == 1
                && root.GetProperty("status").GetString() == status
                && (value.ValueKind == JsonValueKind.Null
                    ? used is null : value.GetDecimal() == used)
                && (time.ValueKind == JsonValueKind.Null
                    ? reset is null : time.GetInt64() == reset);
        }
        catch (JsonException) { return false; }
    }
}
