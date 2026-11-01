using System.Globalization;
using System.Text.RegularExpressions;

namespace MuseLoginShellPoc;

/// <summary>
/// Parses only the visible Muse Settings > General > Usage text in memory.
/// A valid result requires the Settings heading, the free-plan marker, one
/// usage percentage and one weekly-reset date. It never persists page text.
/// </summary>
internal static class MuseVisibleUsageParser
{
    private static readonly Regex Percent = new(
        @"(?<![0-9])(?<used>100|[0-9]{1,2})\s*%\s*使用済み(?![\p{L}])",
        RegexOptions.CultureInvariant | RegexOptions.Compiled,
        TimeSpan.FromMilliseconds(150));
    private static readonly Regex Reset = new(
        @"1\s*週間の上限は\s*(?<month>[0-9]{1,2})\s*月\s*(?<day>[0-9]{1,2})\s*日にリセットされます",
        RegexOptions.CultureInvariant | RegexOptions.Compiled,
        TimeSpan.FromMilliseconds(150));

    internal static bool TryParse(string? pageText, out SyntheticUsageReading? reading)
    {
        reading = null;
        if (string.IsNullOrWhiteSpace(pageText) || pageText.Length > 8192)
            return false;

        // Do not treat a similar-looking string from a normal chat as usage.
        // These are necessary, not sufficient, markers of the settings view.
        var lines = pageText.Replace('\u00A0', ' ').Split(
            new[] { '\r', '\n' },
            StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
        if (!lines.Any(line => line == "利用状況")
            || lines.Count(line => line == "無料プラン") != 1)
            return false;

        var percentages = Percent.Matches(pageText);
        var resets = Reset.Matches(pageText);
        if (percentages.Count != 1 || resets.Count != 1)
            return false;

        if (!int.TryParse(percentages[0].Groups["used"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int used))
            return false;
        if (!int.TryParse(resets[0].Groups["month"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int month)
            || !int.TryParse(resets[0].Groups["day"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int day)
            || month < 1 || month > 12 || day < 1
            // Leap year allows February 29, because the UI omits the year.
            || day > DateTime.DaysInMonth(2024, month))
            return false;

        reading = new SyntheticUsageReading("無料プラン", used, 100-used, month, day);
        return true;
    }
}
