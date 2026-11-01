using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.WinForms;
using System.Drawing;
using System.Net;
using System.Net.Sockets;
using System.Text;
using System.Text.Json;
using System.Windows.Forms;

namespace MuseLoginShellPoc;

/// <summary>
/// CI-only passive response experiment:
/// - New temporary WebView2 profile, never the signed-in Muse profile.
/// - One loopback-only synthetic server, never muse.ai.
/// - No cookie manager, script injection, request-header reads or outbound
///   requests to a provider.
/// - Observe only a single exact localhost response body in memory.
/// </summary>
internal sealed class SyntheticPassiveResponseWindow : Form
{
    private const string Sample = """
        1:{"success":true,"subscription":{"tier":{"name":"Muse Free"},"usage":{"percentUsed":2,"resetsAt":1790634838}}}
        """;
    private const string Page = """
        <!doctype html><html lang="ja"><head><meta charset="utf-8"></head>
        <body><h1>ローカル合成通信</h1>
        <script>
        fetch('/mock-usage', {cache: 'no-store'})
          .then(r => r.text()).then(() => { document.title = 'Synthetic request done'; });
        </script>
        </body></html>
        """;

    private readonly WebView2 browser = new() { Dock = DockStyle.Fill };
    private readonly System.Windows.Forms.Timer timeout = new() { Interval = 20000 };
    private readonly string profile = Path.Combine(Path.GetTempPath(),
        "MusePassiveFixture-" + Guid.NewGuid().ToString("N"));
    private TcpListener? server;
    private bool finished;
    private int acceptedResponses;
    private int port;

    internal SyntheticPassiveResponseWindow()
    {
        Text = "合成WebView2応答検証（実Museへのアクセスなし）";
        Width = 680;
        Height = 410;
        Controls.Add(browser);
        Shown += async (_, _) => await StartAsync();
        timeout.Tick += (_, _) => Finish(false);
        timeout.Start();
        FormClosed += (_, _) => server?.Stop();
    }

    private async Task StartAsync()
    {
        try
        {
            server = new TcpListener(IPAddress.Loopback, 0);
            server.Start();
            port = ((IPEndPoint)server.LocalEndpoint).Port;
            _ = ServeLoopAsync(server);

            // This fixture uses a fresh profile, not MuseLoginShellPoc's
            // persisted login session or any other user's browser.
            var environment = await CoreWebView2Environment.CreateAsync(
                userDataFolder: profile);
            await browser.EnsureCoreWebView2Async(environment);
            var core = browser.CoreWebView2;
            core.Settings.AreDevToolsEnabled = false;
            core.Settings.AreHostObjectsAllowed = false;
            core.Settings.IsWebMessageEnabled = false;
            core.WebResourceResponseReceived += OnResponse;
            core.Navigate("http://127.0.0.1:" + port + "/");
        }
        catch (Exception)
        {
            Finish(false);
        }
    }

    private async Task ServeLoopAsync(TcpListener listener)
    {
        try
        {
            while (!finished)
            {
                var client = await listener.AcceptTcpClientAsync();
                _ = Task.Run(async () =>
                {
                    using (client)
                    {
                        try { await ServeClientAsync(client); }
                        catch (Exception) { /* Synthetic server only. */ }
                    }
                });
            }
        }
        catch (Exception) when (finished)
        {
            // Listener stopped after completing the test.
        }
        catch (Exception)
        {
            if (!IsDisposed) BeginInvoke(() => Finish(false));
        }
    }

    private static async Task ServeClientAsync(TcpClient client)
    {
        using var io = client.GetStream();
        using var limit = new CancellationTokenSource(TimeSpan.FromSeconds(6));
        var bytes = new byte[4096];
        int count = 0;
        while (count < bytes.Length)
        {
            int n = await io.ReadAsync(bytes.AsMemory(count), limit.Token);
            if (n <= 0) return;
            count += n;
            if (Encoding.ASCII.GetString(bytes, 0, count).Contains("\r\n\r\n",
                    StringComparison.Ordinal)) break;
        }
        string firstLine = Encoding.ASCII.GetString(bytes, 0, count)
            .Split("\r\n", StringSplitOptions.None)[0];
        bool usage = firstLine.StartsWith("GET /mock-usage ", StringComparison.Ordinal);
        bool home = firstLine.StartsWith("GET / HTTP/", StringComparison.Ordinal);
        string body = usage ? Sample : home ? Page : "";
        byte[] payload = Encoding.UTF8.GetBytes(body);
        string header = (usage || home ? "HTTP/1.1 200 OK" : "HTTP/1.1 404 Not Found")
            + "\r\nContent-Type: " + (usage ? "text/x-component" : "text/html")
            + "; charset=utf-8\r\nCache-Control: no-store\r\n"
            + "Content-Length: " + payload.Length + "\r\nConnection: close\r\n\r\n";
        byte[] responseHeader = Encoding.ASCII.GetBytes(header);
        await io.WriteAsync(responseHeader, limit.Token);
        await io.WriteAsync(payload, limit.Token);
        await io.FlushAsync(limit.Token);
    }

    private async void OnResponse(object? sender,
        CoreWebView2WebResourceResponseReceivedEventArgs e)
    {
        if (finished || acceptedResponses != 0)
            return;
        // Must match the EXACT synthetic endpoint before touching the body.
        // Do not inspect e.Request.Headers (which can contain credentials
        // in other use cases). Do not read unrelated response bodies.
        if (!Uri.TryCreate(e.Request.Uri, UriKind.Absolute, out var url)
            || url.Scheme != Uri.UriSchemeHttp
            || url.Host != "127.0.0.1"
            || url.Port != port
            || url.AbsolutePath != "/mock-usage"
            || e.Request.Method != "GET"
            || e.Response.StatusCode != 200)
            return;

        acceptedResponses++;
        try
        {
            using Stream? content = await e.Response.GetContentAsync();
            if (content is null) { Finish(false); return; }
            byte[] buffer = new byte[4097];
            int count = 0;
            // Reading from the stream runs off the UI thread, with a strict
            // memory bound. The raw synthetic body is never printed or saved.
            int result = await Task.Run(async () =>
            {
                while (count < buffer.Length)
                {
                    int n = await content.ReadAsync(buffer.AsMemory(count));
                    if (n == 0) break;
                    count += n;
                }
                return count;
            });
            if (result < 1 || result > 4096) { Finish(false); return; }
            string response = Encoding.UTF8.GetString(buffer, 0, result);
            int colon = response.IndexOf(':');
            if (colon <= 0) { Finish(false); return; }
            using var document = JsonDocument.Parse(response[(colon + 1)..]);
            JsonElement sub = document.RootElement
                .GetProperty("subscription");
            bool pass = document.RootElement.GetProperty("success").GetBoolean()
                && sub.GetProperty("tier").GetProperty("name").GetString() == "Muse Free"
                && sub.GetProperty("usage").GetProperty("percentUsed").GetInt32() == 2
                && sub.GetProperty("usage").GetProperty("resetsAt").GetInt64()
                    == 1790634838;
            Finish(pass);
        }
        catch (Exception)
        {
            Finish(false);
        }
    }

    private void Finish(bool pass)
    {
        if (finished) return;
        finished = true;
        timeout.Stop();
        Environment.ExitCode = pass && acceptedResponses == 1 ? 0 : 1;
        server?.Stop();
        Close();
    }
}
