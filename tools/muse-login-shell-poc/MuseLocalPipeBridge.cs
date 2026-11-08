using System.IO.Pipes;
using System.Text;
using System.Text.Json;

namespace MuseLoginShellPoc;

// Self-only, same-Windows-user IPC; never transfers a browser cookie,
// account identity, raw Muse response or HTTP header.
internal sealed class MuseLocalPipeBridge : IDisposable
{
    internal const string DefaultName = "SolidailyMuseQuotaLocalV1";
    private readonly string name;
    private readonly Func<CancellationToken, Task<MuseAutomaticResult>> poll;
    private readonly CancellationTokenSource stopping = new();
    private Task? server;
    private bool disposed;

    internal MuseLocalPipeBridge(Func<CancellationToken, Task<MuseAutomaticResult>> poll,
        string name = DefaultName)
    {
        this.poll = poll;
        this.name = name;
    }

    internal void Start()
    {
        if (disposed) throw new ObjectDisposedException(nameof(MuseLocalPipeBridge));
        server ??= Task.Run(() => ServeAsync(stopping.Token));
    }

    internal void Stop() => stopping.Cancel();

    private async Task ServeAsync(CancellationToken cancel)
    {
        while (!cancel.IsCancellationRequested)
        {
            try
            {
                using var pipe = new NamedPipeServerStream(name,
                    PipeDirection.InOut, 1, PipeTransmissionMode.Byte,
                    PipeOptions.Asynchronous | PipeOptions.CurrentUserOnly, 256, 512);
                await pipe.WaitForConnectionAsync(cancel);
                using var limit = CancellationTokenSource.CreateLinkedTokenSource(cancel);
                limit.CancelAfter(TimeSpan.FromSeconds(90));
                string? request = await ReadCommandAsync(pipe, limit.Token);
                var result = request == "READ"
                    ? await poll(limit.Token)
                    : new MuseAutomaticResult(MuseAutomaticStatus.InvalidResponse);
                byte[] response = Encoding.UTF8.GetBytes(FormatResponse(result) + "\n");
                await pipe.WriteAsync(response, limit.Token);
                await pipe.FlushAsync(limit.Token);
            }
            catch (OperationCanceledException) when (cancel.IsCancellationRequested) { return; }
            catch (OperationCanceledException) { } // Single client timeout.
            catch (IOException) { } // Disconnected client.
            catch (Exception) { return; } // No private errors are logged.
        }
    }

    private static async Task<string?> ReadCommandAsync(Stream pipe, CancellationToken ct)
    {
        var input = new byte[8];
        int size = 0;
        while (size < input.Length)
        {
            int read = await pipe.ReadAsync(input.AsMemory(size, 1), ct);
            if (read == 0) return null;
            size += read;
            if (input[size - 1] == '\n')
                return Encoding.ASCII.GetString(input, 0, size - 1);
        }
        return null;
    }

    internal static string FormatResponse(MuseAutomaticResult result)
    {
        bool ok = result.Status == MuseAutomaticStatus.Success
            && result.Reading is { } quota
            && quota.UsedPercent >= 0 && quota.UsedPercent <= 100;
        return JsonSerializer.Serialize(new
        {
            schema_version = 1,
            status = ok ? "success" : "unavailable",
            used_percent = ok ? result.Reading!.UsedPercent : (decimal?)null,
            resets_at_epoch_seconds = ok
                ? result.Reading!.WeeklyResetAt?.ToUnixTimeSeconds() : null
        });
    }

    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        stopping.Cancel();
        stopping.Dispose();
    }
}
