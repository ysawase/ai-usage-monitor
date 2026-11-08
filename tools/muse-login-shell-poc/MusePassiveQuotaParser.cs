using System.Globalization;
using System.Text.Json;

namespace MuseLoginShellPoc;

internal enum MusePassiveQuotaParseState
{
    Success,
    NoQuota,
    Invalid
}

internal sealed record MusePassiveQuotaReading(
    string Plan, decimal UsedPercent, DateTimeOffset? WeeklyResetAt);

/// <summary>
/// Parse ONLY a bounded React Flight subscription payload already delivered
/// to an opted-in WebView2. This code sends no HTTP requests and reads no cookies.
/// Never log, persist or expose raw payloads.
/// </summary>
internal static class MusePassiveQuotaParser
{
    internal const int MaxBytes = 65_536;

    // Observed in the self-only real Muse response UI on 2026-11-07:
    // UTF-8 Japanese "無料" had been displayed as Windows-1252 mojibake
    // (Museç„¡æ–™). Normalize this one known alias only. Do not attempt
    // broad decoding or infer tier from weekly usage percentage.
    internal static string NormalizePlan(string plan) =>
        plan is "Muse Free" or "Muse無料"
            or "Muse\u00e7\u201e\u00a1\u00e6\u2013\u2122"
            ? "Muse Free"
            : plan;

    internal static MusePassiveQuotaParseState Parse(
        string? raw, out MusePassiveQuotaReading? reading)
    {
        reading = null;
        if (string.IsNullOrEmpty(raw)) return MusePassiveQuotaParseState.NoQuota;
        if (System.Text.Encoding.UTF8.GetByteCount(raw) > MaxBytes)
            return MusePassiveQuotaParseState.Invalid;

        bool seen = false;
        foreach (var line in raw.Split('\n'))
        {
            var trimmed = line.TrimEnd('\r');
            int colon = trimmed.IndexOf(':');
            if (colon < 1 || colon > 8 || colon >= trimmed.Length - 1)
                continue;
            bool flightId = true;
            for (int i = 0; i < colon; i++)
            {
                if (!Uri.IsHexDigit(trimmed[i])) { flightId = false; break; }
            }
            if (!flightId || trimmed[colon + 1] != '{') continue;
            try
            {
                using var document = JsonDocument.Parse(trimmed[(colon + 1)..]);
                var root = document.RootElement;
                if (!root.TryGetProperty("success", out var success)
                    || success.ValueKind != JsonValueKind.True)
                    continue;
                if (seen) return MusePassiveQuotaParseState.Invalid;
                seen = true;
                if (!root.TryGetProperty("subscription", out var subscription)
                    || subscription.ValueKind != JsonValueKind.Object
                    || !subscription.TryGetProperty("tier", out var tier)
                    || tier.ValueKind != JsonValueKind.Object
                    || !tier.TryGetProperty("name", out var name)
                    || name.ValueKind != JsonValueKind.String
                    || !subscription.TryGetProperty("usage", out var usage)
                    || usage.ValueKind != JsonValueKind.Object
                    || !usage.TryGetProperty("percentUsed", out var percent)
                    || percent.ValueKind != JsonValueKind.Number
                    || !percent.TryGetDecimal(out var used)
                    || used < 0 || used > 100)
                    return MusePassiveQuotaParseState.Invalid;
                string? plan = name.GetString();
                if (string.IsNullOrWhiteSpace(plan) || plan.Length > 64
                    || plan.Any(char.IsControl))
                    return MusePassiveQuotaParseState.Invalid;
                DateTimeOffset? reset = null;
                if (usage.TryGetProperty("resetsAt", out var epoch)
                    && epoch.ValueKind != JsonValueKind.Null)
                {
                    if (epoch.ValueKind != JsonValueKind.Number
                        || !epoch.TryGetInt64(out long seconds)
                        || seconds < 946_684_800L || seconds > 4_102_444_800L)
                        return MusePassiveQuotaParseState.Invalid;
                    reset = DateTimeOffset.FromUnixTimeSeconds(seconds);
                }
                reading = new MusePassiveQuotaReading(NormalizePlan(plan), used, reset);
            }
            catch (JsonException)
            {
                // Other Flight objects are not quota results.
                continue;
            }
        }
        return reading is null
            ? MusePassiveQuotaParseState.NoQuota
            : MusePassiveQuotaParseState.Success;
    }
}
