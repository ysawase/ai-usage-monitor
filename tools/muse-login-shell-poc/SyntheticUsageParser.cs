using System.Globalization;
using System.Text.RegularExpressions;

namespace MuseLoginShellPoc;

/// <summary>
/// Strict parser for first-party-style, but entirely SYNTHETIC, visible labels.
/// Does NOT know the user's Muse account, reset year, hour or time zone.
/// </summary>
public sealed record SyntheticUsageReading(
    string Plan,
    int UsedPercent,
    int RemainingPercent,
    int ResetMonth,
    int ResetDay);

public static class SyntheticUsageParser
{
    private static readonly Regex Usage = new(
        @"^(?<percent>[0-9]{1,3})\s*%\s*使用済み$",
        RegexOptions.CultureInvariant | RegexOptions.Compiled);

    private static readonly Regex Reset = new(
        @"^1週間の上限は(?<month>[0-9]{1,2})月(?<day>[0-9]{1,2})日にリセットされます$",
        RegexOptions.CultureInvariant | RegexOptions.Compiled);

    public static bool TryParse(IEnumerable<string>? visibleTexts, out SyntheticUsageReading? reading)
    {
        reading = null;
        if (visibleTexts is null) return false;

        // Bounded UIA collection: never ask for the real WebView2 tree.
        string[] values = visibleTexts.Take(40)
            .Where(text => !string.IsNullOrWhiteSpace(text))
            .Select(text => text.Trim())
            .ToArray();

        int planCount = values.Count(value => value == "無料プラン");
        var usageMatches = values.Select(value => Usage.Match(value))
            .Where(match => match.Success).ToArray();
        var resetMatches = values.Select(value => Reset.Match(value))
            .Where(match => match.Success).ToArray();

        if (planCount != 1 || usageMatches.Length != 1 || resetMatches.Length != 1)
            return false;

        if (!int.TryParse(usageMatches[0].Groups["percent"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int used)
            || used is < 0 or > 100)
            return false;

        if (!int.TryParse(resetMatches[0].Groups["month"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int month)
            || !int.TryParse(resetMatches[0].Groups["day"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int day)
            || month is < 1 or > 12
            || day < 1 || day > DateTime.DaysInMonth(2024, month))
            return false;

        reading = new SyntheticUsageReading("無料プラン", used, 100 - used, month, day);
        return true;
    }
}
