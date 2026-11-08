using System.Net.Http;
using System.Net;
using System.Text;

namespace MuseLoginShellPoc;

// Synthetic only. No Muse credentials, browser profile or real HTTP is used.
internal static class MuseAutomaticQuotaSelfTest
{
    internal static async Task<bool> RunAsync()
    {
        const string action = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        const long reset = 1794584040L;
        var count = 0;
        var cookieAtStatic = false;
        var pageCalls = 0;
        var postCalls = 0;
        using var mock = new FakeHandler(request =>
        {
            count++;
            if (request.RequestUri?.Host != "muse.ai"
                || request.RequestUri.Scheme != "https")
                throw new InvalidOperationException("Request escaped Muse origin");
            var path = request.RequestUri.AbsolutePath;
            if (request.Method == HttpMethod.Get && path == "/")
            {
                pageCalls++;
                if (!request.Headers.Contains("Cookie"))
                    throw new InvalidOperationException("Session missing from page request");
                return Respond(HttpStatusCode.OK,
                    "<html><script src=\"/_next/static/chunks/test.js\"></script></html>",
                    "text/html");
            }
            if (request.Method == HttpMethod.Get
                && path == "/_next/static/chunks/test.js")
            {
                if (request.Headers.Contains("Cookie")) cookieAtStatic = true;
                return Respond(HttpStatusCode.OK,
                    "createServerReference(\"" + action + "\",\"fetchSubscriptionAction\")",
                    "application/javascript");
            }
            if (request.Method == HttpMethod.Post && path == "/")
            {
                postCalls++;
                if (!request.Headers.TryGetValues("Next-Action", out var ids)
                    || ids.Single() != action
                    || !request.Headers.Contains("Cookie"))
                    throw new InvalidOperationException("Wrong subscription request");
                return Respond(HttpStatusCode.OK,
                    "1:{\"success\":true,\"subscription\":{\"tier\":{\"name\":\"Muse Free\"},"
                    + "\"usage\":{\"percentUsed\":7,\"resetsAt\":" + reset + "}}}",
                    "text/x-component");
            }
            throw new InvalidOperationException("Unexpected request");
        });
        using var client = new MuseAutomaticQuotaReader(mock);
        var first = await client.ReadAsync(
            "test_session=synthetic", "Synthetic-Test-Agent", CancellationToken.None);
        var second = await client.ReadAsync(
            "test_session=synthetic", "Synthetic-Test-Agent", CancellationToken.None);
        if (first.Status != MuseAutomaticStatus.Success
            || second.Status != MuseAutomaticStatus.Success
            || first.Reading is not { Plan: "Muse Free", UsedPercent: 7M }
            || first.Reading.WeeklyResetAt?.ToUnixTimeSeconds() != reset
            || cookieAtStatic || pageCalls != 1 || postCalls != 2 || count != 4)
            return false;

        using var reject = new MuseAutomaticQuotaReader(
            new FakeHandler(_ => Respond(
                HttpStatusCode.Unauthorized, "signed out", "text/html")));
        var signedOut = await reject.ReadAsync(
            "test_session=synthetic", null, CancellationToken.None);
        if (signedOut.Status != MuseAutomaticStatus.AuthenticationRequired)
            return false;
        using var throttled = new MuseAutomaticQuotaReader(
            new FakeHandler(_ => Respond(
                HttpStatusCode.TooManyRequests, "slow", "text/html")));
        var rate = await throttled.ReadAsync(
            "test_session=synthetic", null, CancellationToken.None);
        return rate.Status == MuseAutomaticStatus.RateLimited
            && (await client.ReadAsync(null, null, CancellationToken.None)).Status
                == MuseAutomaticStatus.NoSession;
    }

    private static HttpResponseMessage Respond(
        HttpStatusCode code, string content, string mediaType) =>
        new(code) { Content = new StringContent(content, Encoding.UTF8, mediaType) };

    private sealed class FakeHandler(
        Func<HttpRequestMessage, HttpResponseMessage> respond) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(
            HttpRequestMessage request, CancellationToken cancellationToken) =>
            Task.FromResult(respond(request));
    }
}
