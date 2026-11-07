using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.WinForms;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Windows.Forms;

namespace MuseLoginShellPoc;

// Isolated synthetic diagnostic: localhost only, temporary browser profile.
// Tests specific WebView2 response stages. Never uses real Muse or cookies.
internal sealed class PassiveResponseDiagnostic : Form
{
    // Deliberately exceeds 512 bytes to exercise multiple stream reads.
    private static readonly string SyntheticFixture =
        @"1:{""success"":true,""subscription"":{""tier"":{""name"":""Muse Free""},""usage"":{""percentUsed"":2,""resetsAt"":1790634838}},""syntheticPadding"":""" +
        new string('x', 1200) +
        @"""}";
    private readonly WebView2 view = new() { Dock = DockStyle.Fill };
    private readonly System.Windows.Forms.Timer timeout = new() { Interval = 18000 };
    private TcpListener? listener;
    private string stage = "start";
    private bool done;
    private int port;
    private int matched;
    private static readonly string LogPath = Path.Combine(
        Path.GetTempPath(), "MusePassiveResponseStage.txt");

    internal PassiveResponseDiagnostic()
    {
        Controls.Add(view);
        Shown += async (_, _) => await StartAsync();
        timeout.Tick += (_, _) => Finish(false, "timeout");
        timeout.Start();
    }

    private async Task StartAsync()
    {
        try
        {
            listener = new TcpListener(IPAddress.Loopback, 0);
            listener.Start();
            port = ((IPEndPoint)listener.LocalEndpoint).Port;
            _ = ServeAsync(listener);
            stage = "environment";
            var env = await CoreWebView2Environment.CreateAsync(
                userDataFolder: Path.Combine(Path.GetTempPath(),
                    "MusePassiveDiag-" + Guid.NewGuid().ToString("N")));
            await view.EnsureCoreWebView2Async(env);
            view.CoreWebView2.WebResourceResponseReceived += OnResponse;
            stage = "navigate";
            view.CoreWebView2.Navigate($"http://127.0.0.1:{port}/");
        }
        catch (Exception ex)
        {
            Finish(false, "init_" + ex.GetType().Name);
        }
    }

    private async Task ServeAsync(TcpListener server)
    {
        try
        {
            while (!done)
            {
                using var client = await server.AcceptTcpClientAsync();
                try
                {
                using var stream = client.GetStream();
                using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
                byte[] buf = new byte[4096];
                int count = 0;
                while (count < buf.Length)
                {
                    int n = await stream.ReadAsync(buf.AsMemory(count), cts.Token);
                    if (n == 0) break;
                    count += n;
                    if (Encoding.ASCII.GetString(buf, 0, count).Contains("\r\n\r\n"))
                        break;
                }
                string line = Encoding.ASCII.GetString(buf, 0, count).Split("\r\n")[0];
                bool usage = line.StartsWith("GET /test ", StringComparison.Ordinal);
                string body = usage ? SyntheticFixture :
                    "<!doctype html><html><body><script>fetch('/test')</script></body></html>";
                byte[] bytes = Encoding.UTF8.GetBytes(body);
                byte[] head = Encoding.ASCII.GetBytes(
                    $"HTTP/1.1 200 OK\r\nContent-Type: {(usage ? "text/x-component" : "text/html")}; charset=utf-8\r\nContent-Length: {bytes.Length}\r\nConnection: close\r\n\r\n");
                await stream.WriteAsync(head, cts.Token);
                await stream.WriteAsync(bytes, cts.Token);
                }
                catch (IOException) { /* Ignore closed synthetic connections. */ }
                catch (OperationCanceledException) { /* Per-connection bound. */ }
            }
        }
        catch (Exception) when (done) { }
        catch (Exception ex) { Finish(false, "server_" + ex.GetType().Name); }
    }

    private async void OnResponse(object? sender,
        CoreWebView2WebResourceResponseReceivedEventArgs e)
    {
        if (done || matched != 0) return;
        if (!Uri.TryCreate(e.Request.Uri, UriKind.Absolute, out var url)
            || url.Host != "127.0.0.1" || url.Port != port
            || url.AbsolutePath != "/test" || e.Response.StatusCode != 200)
            return;
        matched++;
        try
        {
            stage = "before_getcontent";
            Stream? body = await e.Response.GetContentAsync();
            stage = "after_getcontent";
            if (body is null) { Finish(false, "null_stream"); return; }
            // Check the stream's declared capabilities before attempting Read.
            stage = body.CanRead ? "readable" : "unreadable";
            // Read through EOF with an explicit cap. A single Stream.Read is not guaranteed
            // to return the complete response, even when the content is available.
            byte[] all = await Task.Run(() =>
            {
                using var output = new MemoryStream();
                byte[] chunk = new byte[256];
                while (true)
                {
                    int count = body.Read(chunk, 0, chunk.Length);
                    if (count == 0) break;
                    if (output.Length > 64 * 1024 - count)
                        throw new InvalidDataException("Synthetic response exceeds 64 KiB");
                    output.Write(chunk, 0, count);
                }
                return output.ToArray();
            });
            stage = "after_read";
            bool valid = Encoding.UTF8.GetString(all) == SyntheticFixture;
            // Exercise the very same parser used by the opt-in Muse window
            // against the actual WebView2-delivered synthetic response.
            var parseState = MusePassiveQuotaParser.Parse(
                Encoding.UTF8.GetString(all), out var parsed);
            valid = valid && parseState == MusePassiveQuotaParseState.Success
                && parsed is { Plan: "Muse Free", UsedPercent: 2M }
                && parsed.WeeklyResetAt?.ToUnixTimeSeconds() == 1790634838L;
            // Fail closed on malformed, missing, duplicate and out-of-range readings.
            valid = valid && MusePassiveQuotaParser.Parse(
                "1:{\"success\":true,\"subscription\":{\"tier\":{\"name\":\"Free\"},\"usage\":{\"percentUsed\":105}}}",
                out _) == MusePassiveQuotaParseState.Invalid
                && MusePassiveQuotaParser.Parse("1:{}", out _)
                    == MusePassiveQuotaParseState.NoQuota;
            // Disposal is separately classified: some COM wrappers may not
            // support methods commonly expected from managed streams.
            try { body.Dispose(); }
            catch (Exception ex) { Finish(false, "dispose_" + ex.GetType().Name); return; }
            Finish(valid, valid ? "verified" : "mismatch");
        }
        catch (Exception ex)
        {
            Finish(false, stage + "_" + ex.GetType().Name);
        }
    }

    private void Finish(bool success, string reason)
    {
        if (done) return;
        done = true;
        timeout.Stop();
        try { File.WriteAllText(LogPath, reason + "; matched=" + matched); }
        catch { }
        Environment.ExitCode = success && matched == 1 ? 0 : 1;
        listener?.Stop();
        if (!IsDisposed && IsHandleCreated) BeginInvoke(Close);
    }
}
