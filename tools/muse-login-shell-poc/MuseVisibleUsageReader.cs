using System.Windows.Automation;
using System.Windows.Automation.Text;

namespace MuseLoginShellPoc;

internal enum MuseVisibleProbeStatus
{
    Success,
    NoDocument,
    MultipleDocuments,
    NoTextPattern,
    SettingsNotRecognized,
    ReadFailed
}

internal sealed record MuseVisibleProbeResult(
    MuseVisibleProbeStatus Status, SyntheticUsageReading? Reading, MuseUsageParseStatus? ParseStatus = null,
    string? SafeDiagnostic = null);

/// <summary>
/// User-initiated, single-shot, local UI Automation. Never navigates Muse,
/// reads cookies, requests a server endpoint, logs document text, or saves
/// an account/quota reading. Runs on a worker thread to avoid UIA deadlocks.
/// </summary>
internal static class MuseVisibleUsageReader
{
    internal static MuseVisibleProbeResult ReadOnce(IntPtr ownerWindowHandle)
    {
        try
        {
            // HWND belongs to the dedicated WinForms Muse PoC, never the desktop
            // root, Chrome, Opera, Edge, or an unrelated window.
            var window = AutomationElement.FromHandle(ownerWindowHandle);
            var documents = window.FindAll(TreeScope.Descendants,
                new PropertyCondition(AutomationElement.ControlTypeProperty,
                    ControlType.Document));

            // Avoid concatenating unrelated documents/frames or chat content.
            if (documents.Count == 0)
                return new(MuseVisibleProbeStatus.NoDocument, null);
            if (documents.Count != 1)
                return new(MuseVisibleProbeStatus.MultipleDocuments, null);
            if (!documents[0].TryGetCurrentPattern(
                    TextPattern.Pattern, out object pattern)
                || pattern is not TextPattern textPattern)
                return new(MuseVisibleProbeStatus.NoTextPattern, null);

            // Search the text provider directly: a long background chat can
            // place the Settings sheet beyond the first 8192 characters.
            TextPatternRange document = textPattern.DocumentRange;
            TextPatternRange? heading = document.FindText(
                "利用状況", backward: true, ignoreCase: false);
            if (heading is null)
                return new(MuseVisibleProbeStatus.SettingsNotRecognized, null,
                    MuseUsageParseStatus.MissingUsageSection);
            TextPatternRange usageRange = heading.Clone();
            usageRange.MoveEndpointByRange(TextPatternRangeEndpoint.End,
                document, TextPatternRangeEndpoint.End);
            string usageSnippet = "利用状況" + usageRange.GetText(768);
            // Only the bounded snippet is materialized, never logged or saved.
            if (!MuseVisibleUsageParser.TryParse(
                    usageSnippet, out var reading, out var parseStatus))
            {
                // Diagnostic metadata only. FindText returns a range but its
                // contents and coordinates are never read, persisted or sent.
                // A hit in the document may be unrelated chat content: this
                // is NOT a source of valid quota values.
                string? diagnostic = null;
                if (parseStatus == MuseUsageParseStatus.MissingPlan)
                {
                    bool planAnywhere = document.FindText(
                        "無料プラン", backward: true, ignoreCase: false) is not null;
                    bool percentAnywhere = document.FindText(
                        "使用済み", backward: true, ignoreCase: false) is not null;
                    diagnostic = "lengthCap=" + (usageSnippet.Length >= 770 ? "yes" : "no")
                        + ";planAnywhere=" + (planAnywhere ? "yes" : "no")
                        + ";percentAnywhere=" + (percentAnywhere ? "yes" : "no");
                }
                // UIA can expose visible overlay labels before the heading.
                // Attempt one bounded backward window, never the full document.
                if (parseStatus == MuseUsageParseStatus.MissingPlan)
                {
                    TextPatternRange near = heading.Clone();
                    near.MoveEndpointByUnit(TextPatternRangeEndpoint.Start,
                        TextUnit.Character, -240);
                    near.MoveEndpointByRange(TextPatternRangeEndpoint.End,
                        heading, TextPatternRangeEndpoint.Start);
                    string before = near.GetText(240);
                    // Exactly one heading, at most 240 chars on each side.
                    // Do not extend the fallback into background chat text.
                    string after = usageSnippet[..Math.Min(usageSnippet.Length, 244)];
                    string combined = before + " " + after;
                    if (MuseVisibleUsageParser.TryParseNearHeading(
                        combined, out var nearbyReading, out _)
                        && nearbyReading is not null)
                        return new(MuseVisibleProbeStatus.Success, nearbyReading);
                }
                return new(MuseVisibleProbeStatus.SettingsNotRecognized,
                    null, parseStatus, diagnostic);
            }
            return new(MuseVisibleProbeStatus.Success, reading);
        }
        catch (Exception)
        {
            // Do not stringify exception details which could contain page info.
            return new(MuseVisibleProbeStatus.ReadFailed, null);
        }
    }
}
