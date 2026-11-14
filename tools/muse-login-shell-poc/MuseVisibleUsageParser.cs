using System.Globalization;
using System.Text;
using System.Text.RegularExpressions;

namespace MuseLoginShellPoc;

internal enum MuseUsageParseStatus
{
    Success,
    MissingUsageSection,
    MissingPlan,
    MissingPercent,
    MissingReset,
    AmbiguousOrInvalid
}

/// <summary>
/// In-memory parsing of visible Muse Settings > General > Usage labels.
/// TextPattern can join adjacent HTML nodes without newline boundaries.
/// Restrict matching to a short span after the Usage heading, never the full
/// chat/background, and fail closed on ambiguity or invalid values.
/// </summary>
internal static class MuseVisibleUsageParser
{
    private static readonly Regex Percent = new(
        @"(?<![0-9])(?<used>100|[0-9]{1,2})\s*[%％]\s*使用済み",
        RegexOptions.CultureInvariant | RegexOptions.Compiled,
        TimeSpan.FromMilliseconds(150));
    private static readonly Regex Reset = new(
        @"1\s*週間の上限は\s*(?<month>[0-9]{1,2})\s*月\s*(?<day>[0-9]{1,2})\s*日にリセットされます",
        RegexOptions.CultureInvariant | RegexOptions.Compiled,
        TimeSpan.FromMilliseconds(150));

    internal static bool TryParse(string? text, out SyntheticUsageReading? reading) =>
        TryParse(text, out reading, out _);

    internal static bool TryParse(
        string? text, out SyntheticUsageReading? reading, out MuseUsageParseStatus status)
    {
        reading = null;
        status = MuseUsageParseStatus.AmbiguousOrInvalid;
        if (string.IsNullOrWhiteSpace(text) || text.Length > 8192)
            return false;

        // The real Muse modal can appear after the chat content. Do not log
        // the original or normalized document, even for failed parsing.
        // WebView2 accessibility text may insert Unicode direction/zero-width
        // format marks inside "無料プラン". Ignore only format characters and
        // normalize width; do not infer a missing plan or read other documents.
        string normalized = new string(text.Normalize(NormalizationForm.FormKC)
            .Where(c => CharUnicodeInfo.GetUnicodeCategory(c) != UnicodeCategory.Format)
            .ToArray()).Replace('\u00a0', ' ');
        const string heading = "利用状況";
        int start = normalized.LastIndexOf(heading, StringComparison.Ordinal);
        if (start < 0)
        {
            status = MuseUsageParseStatus.MissingUsageSection;
            return false;
        }
        // Usage panel content in the UI is compact. Never parse values
        // elsewhere in the document, such as from a chat containing "2%".
        int end = Math.Min(normalized.Length, start + 512);
        string usageArea = normalized.Substring(start + heading.Length, end - start - heading.Length);

        int planCount = Regex.Matches(
            usageArea, "無料\\s*プラン", RegexOptions.CultureInvariant,
            TimeSpan.FromMilliseconds(150)).Count;
        if (planCount == 0)
        {
            status = MuseUsageParseStatus.MissingPlan;
            return false;
        }
        if (planCount != 1)
            return false;

        var usages = Percent.Matches(usageArea);
        if (usages.Count == 0)
        {
            status = MuseUsageParseStatus.MissingPercent;
            return false;
        }
        if (usages.Count != 1)
            return false;

        var resets = Reset.Matches(usageArea);
        if (resets.Count == 0)
        {
            status = MuseUsageParseStatus.MissingReset;
            return false;
        }
        if (resets.Count != 1)
            return false;

        if (!int.TryParse(usages[0].Groups["used"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int used)
            || used is < 0 or > 100
            || !int.TryParse(resets[0].Groups["month"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int month)
            || !int.TryParse(resets[0].Groups["day"].Value,
                NumberStyles.None, CultureInfo.InvariantCulture, out int day)
            || month is < 1 or > 12 || day < 1
            || day > DateTime.DaysInMonth(2024, month))
            return false;

        reading = new SyntheticUsageReading("無料プラン", used, 100-used, month, day);
        status = MuseUsageParseStatus.Success;
        return true;
    }
}
