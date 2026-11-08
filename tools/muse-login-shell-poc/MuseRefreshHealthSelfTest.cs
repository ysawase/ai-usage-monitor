using System.Text.Json;

namespace MuseLoginShellPoc;

// Synthetic, offline-only and private: never loads WebView2 or Muse credentials.
internal static class MuseRefreshHealthSelfTest
{
    internal static bool Run()
    {
        string root = Path.Combine(Path.GetTempPath(),
            "MuseRefreshHealthSynthetic-" + Guid.NewGuid().ToString("N"));
        try
        {
            var recorder = new MuseRefreshHealth(root);
            var first = new DateTimeOffset(2026, 11, 8, 1, 20, 0, TimeSpan.Zero);
            recorder.Start(first);
            recorder.Attempt(first);
            recorder.Success(first.AddSeconds(2));
            recorder.Attempt(first.AddMinutes(15));
            recorder.Success(first.AddMinutes(15).AddSeconds(2));
            if (!Check(recorder.StatusPath, "running", 2, "success",
                    "2026-11-08T01:20:02+00:00",
                    "2026-11-08T01:35:02+00:00")) return false;
            recorder.Attempt(first.AddMinutes(30));
            recorder.Failure(first.AddMinutes(30), "RateLimited");
            recorder.Stop(first.AddMinutes(30));
            if (!Check(recorder.StatusPath, "stopped", 2, "rate_limited",
                    "2026-11-08T01:20:02+00:00",
                    "2026-11-08T01:35:02+00:00")) return false;

            // A fresh opt-in session never inherits a prior run's two passes.
            recorder.Start(first.AddHours(1));
            if (!Check(recorder.StatusPath, "running", 0, "not_checked",
                    null, null)) return false;
            recorder.Stop(first.AddHours(1).AddSeconds(1));
            return Check(recorder.StatusPath, "stopped", 0, "cancelled",
                null, null);
        }
        finally
        {
            try { Directory.Delete(root, recursive: true); } catch { }
        }
    }

    private static bool Check(string path, string state, int count,
        string outcome, string? previous, string? last)
    {
        if (!File.Exists(path)) return false;
        string raw = File.ReadAllText(path);
        // Contains metadata only. Do not accidentally expand to plan,
        // quota %, cookies, tokens or entire subscription responses.
        foreach (var forbidden in new[]
        {
            "Cookie", "Authorization", "subscription", "used_percent",
            "quota", "Muse Free", "token", "test_session", "7%"
        })
        {
            if (raw.Contains(forbidden, StringComparison.OrdinalIgnoreCase))
                return false;
        }
        using var parsed = JsonDocument.Parse(raw);
        var json = parsed.RootElement;
        return json.GetProperty("schema_version").GetInt32() == 1
            && json.GetProperty("state").GetString() == state
            && json.GetProperty("success_count_in_session").GetInt32() == count
            && json.GetProperty("last_outcome").GetString() == outcome
            && ExtractDate(json, "previous_success_utc") == previous
            && ExtractDate(json, "last_success_utc") == last;
    }

    private static string? ExtractDate(JsonElement root, string key) =>
        root.GetProperty(key).ValueKind == JsonValueKind.Null
            ? null : root.GetProperty(key).GetDateTimeOffset()
                .ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ss+00:00");
}
