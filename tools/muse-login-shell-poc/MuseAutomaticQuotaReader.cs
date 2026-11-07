using System.Net.Http;
using System.Net;
using System.Net.Http.Headers;
using System.Text;
using System.Text.RegularExpressions;

namespace MuseLoginShellPoc;

// PERSONAL / SELF-ONLY EXPERIMENT. Never included in the Rust Store application.
// The owner opts into refresh using ONLY the dedicated WebView2 Muse session.
// No browser-profile search, disk credential storage, extension, telemetry or relay.
internal enum MuseAutomaticStatus
{
    Success,
    NoSession,
    AuthenticationRequired,
    ActionChanged,
    RateLimited,
    InvalidResponse,
    NetworkFailure,
    TimedOut
}

internal sealed record MuseAutomaticResult(
    MuseAutomaticStatus Status, MusePassiveQuotaReading? Reading = null);

internal sealed class MuseAutomaticQuotaReader : IDisposable
{
    private const string Origin = "https://muse.ai";
    private const int PageMaxBytes = 1_048_576;
    private const int ChunkMaxBytes = 1_048_576;
    private const int TotalDiscoveryBytes = 8 * 1024 * 1024;
    private const int MaxPageChunks = 96;
    private const int MaxSettingsChunks = 32;
    private const int BatchSize = 8;

    private readonly HttpClient http;
    private string? cachedActionID;

    private static readonly Regex ChunkPaths = new(
        @"static/chunks/[\w~.-]+\.js", RegexOptions.Compiled | RegexOptions.CultureInvariant,
        TimeSpan.FromMilliseconds(350));
    private static readonly Regex ActionId = new(
        @"""([0-9a-f]{40,})"",[^""]{0,200}""fetchSubscriptionAction""",
        RegexOptions.Compiled | RegexOptions.CultureInvariant,
        TimeSpan.FromMilliseconds(350));
    private static readonly Regex SettingsModule = new(
        @"\.A\((\d+)\)\.then\(\(\{[^}]*Settings",
        RegexOptions.Compiled | RegexOptions.CultureInvariant,
        TimeSpan.FromMilliseconds(350));

    internal MuseAutomaticQuotaReader(HttpMessageHandler? handler = null)
    {
        // Redirects are disabled to ensure the Muse cookie header cannot
        // be forwarded to an authentication or unrelated hostname.
        http = new HttpClient(handler ?? new HttpClientHandler
        {
            AllowAutoRedirect = false,
            UseCookies = false,
            AutomaticDecompression =
                DecompressionMethods.GZip | DecompressionMethods.Deflate
        })
        { Timeout = TimeSpan.FromSeconds(45) };
    }

    internal async Task<MuseAutomaticResult> ReadAsync(
        string? museOnlyCookieHeader, string? browserUserAgent,
        CancellationToken cancellationToken)
    {
        // Never store the cookie in a field or log it.
        if (string.IsNullOrEmpty(museOnlyCookieHeader))
            return new(MuseAutomaticStatus.NoSession);
        try
        {
            if (cachedActionID is not null)
            {
                var cached = await CallSubscriptionAsync(
                    cachedActionID, museOnlyCookieHeader, browserUserAgent, cancellationToken);
                if (cached.Status == MuseAutomaticStatus.Success)
                    return cached;
                if (cached.Status != MuseAutomaticStatus.ActionChanged)
                    return cached;
                cachedActionID = null;  // Rediscover only once.
            }

            string newID = await DiscoverActionIDAsync(
                museOnlyCookieHeader, browserUserAgent, cancellationToken);
            var fresh = await CallSubscriptionAsync(
                newID, museOnlyCookieHeader, browserUserAgent, cancellationToken);
            if (fresh.Status == MuseAutomaticStatus.Success)
                cachedActionID = newID;  // Memory only, and only after a valid response.
            return fresh;
        }
        catch (OperationCanceledException)
        {
            return new(MuseAutomaticStatus.TimedOut);
        }
        catch (MusePollException ex)
        {
            return new(ex.Status);
        }
        catch (HttpRequestException)
        {
            return new(MuseAutomaticStatus.NetworkFailure);
        }
        catch (IOException)
        {
            return new(MuseAutomaticStatus.NetworkFailure);
        }
        catch (RegexMatchTimeoutException)
        {
            return new(MuseAutomaticStatus.ActionChanged);
        }
        catch (Exception)
        {
            // Exceptions may contain URLs or headers; never surface the body.
            return new(MuseAutomaticStatus.InvalidResponse);
        }
    }

    private async Task<MuseAutomaticResult> CallSubscriptionAsync(
        string actionID, string cookie, string? userAgent, CancellationToken ct)
    {
        using var request = NewRequest(HttpMethod.Post, new Uri(Origin + "/"), userAgent);
        request.Headers.TryAddWithoutValidation("Cookie", cookie);
        request.Headers.TryAddWithoutValidation("Next-Action", actionID);
        request.Headers.TryAddWithoutValidation("Origin", Origin);
        request.Headers.TryAddWithoutValidation("Sec-Fetch-Site", "same-origin");
        request.Headers.TryAddWithoutValidation("Sec-Fetch-Mode", "cors");
        request.Headers.TryAddWithoutValidation("Sec-Fetch-Dest", "empty");
        request.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("text/x-component"));
        request.Content = new StringContent(
            "[{\"includeAgreement\":true}]", Encoding.UTF8, "application/json");

        using var response = await http.SendAsync(
            request, HttpCompletionOption.ResponseHeadersRead, ct);
        if (response.StatusCode == HttpStatusCode.TooManyRequests)
            return new(MuseAutomaticStatus.RateLimited);
        if (IsSignedOut(response))
            return new(MuseAutomaticStatus.AuthenticationRequired);
        if (response.StatusCode == HttpStatusCode.NotFound)
        {
            var stale = await ReadLimitedAsync(response.Content, 256, ct);
            return new(Encoding.UTF8.GetString(stale)
                .Contains("Server action not found", StringComparison.OrdinalIgnoreCase)
                ? MuseAutomaticStatus.ActionChanged : MuseAutomaticStatus.NetworkFailure);
        }
        if (response.StatusCode != HttpStatusCode.OK)
            return new(MuseAutomaticStatus.NetworkFailure);
        if (response.Content.Headers.ContentType?.MediaType != "text/x-component")
            return new(MuseAutomaticStatus.InvalidResponse);
        var raw = await ReadLimitedAsync(
            response.Content, MusePassiveQuotaParser.MaxBytes, ct);
        var state = MusePassiveQuotaParser.Parse(
            Encoding.UTF8.GetString(raw), out var reading);
        return state == MusePassiveQuotaParseState.Success && reading is not null
            ? new(MuseAutomaticStatus.Success, reading)
            : new(MuseAutomaticStatus.InvalidResponse);
    }

    private async Task<string> DiscoverActionIDAsync(
        string cookie, string? userAgent, CancellationToken ct)
    {
        using var req = NewRequest(HttpMethod.Get, new Uri(Origin + "/"), userAgent);
        req.Headers.TryAddWithoutValidation("Cookie", cookie);
        req.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("text/html"));
        using var pageResponse = await http.SendAsync(
            req, HttpCompletionOption.ResponseHeadersRead, ct);
        if (IsSignedOut(pageResponse))
            throw new MusePollException(MuseAutomaticStatus.AuthenticationRequired);
        if (pageResponse.StatusCode == HttpStatusCode.TooManyRequests)
            throw new MusePollException(MuseAutomaticStatus.RateLimited);
        if (pageResponse.StatusCode != HttpStatusCode.OK)
            throw new MusePollException(MuseAutomaticStatus.NetworkFailure);

        var html = Encoding.UTF8.GetString(
            await ReadLimitedAsync(pageResponse.Content, PageMaxBytes, ct));
        var paths = ExtractPaths(html).Take(MaxPageChunks).ToArray();
        if (paths.Length == 0)
            throw new MusePollException(MuseAutomaticStatus.ActionChanged);

        // Equivalent bounded discovery path to the public CodexBar provider:
        // page chunks -> settings lazy loader -> settings chunks -> action ID.
        // Static resources are fetched WITHOUT session cookies.
        int seenBytes = 0;
        string loaderText = "";
        for (int i = 0; i < paths.Length; i += BatchSize)
        {
            var chunkBodies = await FetchStaticBatchAsync(
                paths.Skip(i).Take(BatchSize), userAgent, ct);
            foreach (var body in chunkBodies)
            {
                seenBytes = checked(seenBytes + Encoding.UTF8.GetByteCount(body));
                if (seenBytes > TotalDiscoveryBytes)
                    throw new MusePollException(MuseAutomaticStatus.ActionChanged);
                var direct = ExtractActionID(body);
                if (direct is not null) return direct;
                loaderText += "\n" + body;
            }
            var module = SettingsModule.Match(loaderText);
            if (!module.Success) continue;
            var moduleID = Regex.Escape(module.Groups[1].Value);
            var loaderPattern = new Regex(
                @"[,\[]" + moduleID +
                @",\w+=>\{\w+\.v\(\w+=>Promise\.all\(\[([^\]]*)",
                RegexOptions.CultureInvariant, TimeSpan.FromMilliseconds(350));
            var loader = loaderPattern.Match(loaderText);
            if (!loader.Success) continue;

            var settingsPaths = ExtractPaths(loader.Groups[1].Value)
                .Take(MaxSettingsChunks).ToArray();
            for (int j = 0; j < settingsPaths.Length; j += BatchSize)
            {
                var settingsChunks = await FetchStaticBatchAsync(
                    settingsPaths.Skip(j).Take(BatchSize), userAgent, ct);
                foreach (var body in settingsChunks)
                {
                    seenBytes = checked(seenBytes + Encoding.UTF8.GetByteCount(body));
                    if (seenBytes > TotalDiscoveryBytes)
                        throw new MusePollException(MuseAutomaticStatus.ActionChanged);
                    var id = ExtractActionID(body);
                    if (id is not null) return id;
                }
            }
            break;
        }
        throw new MusePollException(MuseAutomaticStatus.ActionChanged);
    }

    private async Task<string[]> FetchStaticBatchAsync(
        IEnumerable<string> paths, string? userAgent, CancellationToken ct)
    {
        return await Task.WhenAll(paths.Select(async path =>
        {
            try
            {
                // Paths originate from a strict static-chunk regex, not
                // arbitrary user-controlled URLs. The origin is hard-coded.
                using var request = NewRequest(
                    HttpMethod.Get, new Uri(Origin + path), userAgent);
                using var response = await http.SendAsync(
                    request, HttpCompletionOption.ResponseHeadersRead, ct);
                if (response.StatusCode != HttpStatusCode.OK) return "";
                return Encoding.UTF8.GetString(
                    await ReadLimitedAsync(response.Content, ChunkMaxBytes, ct));
            }
            catch (OperationCanceledException) when (ct.IsCancellationRequested)
            {
                throw;
            }
            catch (Exception)
            {
                // A missing/oversized static chunk is not itself a successful
                // discovery and is never retried indefinitely.
                return "";
            }
        }));
    }

    private static string[] ExtractPaths(string text) =>
        ChunkPaths.Matches(text)
            .Select(match => "/_next/" + match.Value)
            .Distinct(StringComparer.Ordinal).ToArray();

    private static string? ExtractActionID(string text)
    {
        var match = ActionId.Match(text);
        return match.Success ? match.Groups[1].Value : null;
    }

    private static HttpRequestMessage NewRequest(
        HttpMethod method, Uri uri, string? userAgent)
    {
        // No dynamic redirect, no arbitrary host, and no HTTP downgrade.
        if (uri.Scheme != Uri.UriSchemeHttps ||
            uri.Host != "muse.ai" || uri.Port != 443)
            throw new MusePollException(MuseAutomaticStatus.NetworkFailure);
        var req = new HttpRequestMessage(method, uri);
        if (!string.IsNullOrWhiteSpace(userAgent) && userAgent.Length <= 400 &&
            !userAgent.Contains('\r') && !userAgent.Contains('\n'))
            req.Headers.TryAddWithoutValidation("User-Agent", userAgent);
        return req;
    }

    private static bool IsSignedOut(HttpResponseMessage response)
    {
        int code = (int)response.StatusCode;
        return code is 301 or 302 or 303 or 307 or 308 or 401 or 403;
    }

    private static async Task<byte[]> ReadLimitedAsync(
        HttpContent content, int maxBytes, CancellationToken ct)
    {
        if (content.Headers.ContentLength > maxBytes)
            throw new MusePollException(MuseAutomaticStatus.InvalidResponse);
        await using var stream = await content.ReadAsStreamAsync(ct);
        using var buffer = new MemoryStream();
        byte[] chunk = new byte[4096];
        while (true)
        {
            int n = await stream.ReadAsync(chunk, ct);
            if (n == 0) break;
            if (buffer.Length + n > maxBytes)
                throw new MusePollException(MuseAutomaticStatus.InvalidResponse);
            buffer.Write(chunk, 0, n);
        }
        return buffer.ToArray();
    }

    public void Dispose() => http.Dispose();

    private sealed class MusePollException : Exception
    {
        internal MuseAutomaticStatus Status { get; }
        internal MusePollException(MuseAutomaticStatus status) => Status = status;
    }
}
