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
    MuseVisibleProbeStatus Status, SyntheticUsageReading? Reading, MuseUsageParseStatus? ParseStatus = null);

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
            string usageSnippet = usageRange.GetText(768);
            // Only the bounded snippet is materialized, never logged or saved.
            if (!MuseVisibleUsageParser.TryParse(
                    usageSnippet, out var reading, out var parseStatus))
                return new(MuseVisibleProbeStatus.SettingsNotRecognized, null, parseStatus);
            return new(MuseVisibleProbeStatus.Success, reading);
        }
        catch (Exception)
        {
            // Do not stringify exception details which could contain page info.
            return new(MuseVisibleProbeStatus.ReadFailed, null);
        }
    }
}
