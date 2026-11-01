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

            // The settings modal overlays a possibly very long chat.
            // Find only explicit fixed-label anchors inside the UIA provider.
            // Never copy the entire conversation to locate the usage panel.
            TextPatternRange document = textPattern.DocumentRange;
            TextPatternRange? heading = document.FindText(
                "利用状況", backward: true, ignoreCase: false);
            if (heading is null)
                return new(MuseVisibleProbeStatus.SettingsNotRecognized, null,
                    MuseUsageParseStatus.MissingUsageSection);
            TextPatternRange? plan = document.FindText(
                "無料プラン", backward: true, ignoreCase: false);
            if (plan is null)
                return new(MuseVisibleProbeStatus.SettingsNotRecognized, null,
                    MuseUsageParseStatus.MissingPlan);
            TextPatternRange? used = document.FindText(
                "使用済み", backward: true, ignoreCase: false);
            if (used is null)
                return new(MuseVisibleProbeStatus.SettingsNotRecognized, null,
                    MuseUsageParseStatus.MissingPercent);
            TextPatternRange? reset = document.FindText(
                "リセットされます", backward: true, ignoreCase: false);
            if (reset is null)
                return new(MuseVisibleProbeStatus.SettingsNotRecognized, null,
                    MuseUsageParseStatus.MissingReset);

            // A chat message containing the same words must not be treated
            // as verified quota. All four labels must be visibly located
            // close to the Usage heading within this single test window.
            if (!WithinVisibleUsagePanel(heading, plan, used, reset))
                return new(MuseVisibleProbeStatus.SettingsNotRecognized, null,
                    MuseUsageParseStatus.AmbiguousOrInvalid);

            // Read only short fragments around the matched labels. Each is
            // transient and is never logged, stored, sent or copied.
            string usedFragment = SmallPrefix(used, 8, 64);
            string resetFragment = SmallPrefix(reset, 45, 112);
            string selected = "利用状況\n無料プラン\n"
                + usedFragment + "\n" + resetFragment;
            if (!MuseVisibleUsageParser.TryParse(
                    selected, out var reading, out var parseStatus))
                return new(MuseVisibleProbeStatus.SettingsNotRecognized, null,
                    parseStatus);
            return new(MuseVisibleProbeStatus.Success, reading);
        }
        catch (Exception)
        {
            // Do not stringify exception details which could contain page info.
            return new(MuseVisibleProbeStatus.ReadFailed, null);
        }
    }

    private static string SmallPrefix(TextPatternRange token, int before, int maxChars)
    {
        var part = token.Clone();
        part.MoveEndpointByUnit(TextPatternRangeEndpoint.Start,
            TextUnit.Character, -before);
        return part.GetText(maxChars);
    }

    private static bool WithinVisibleUsagePanel(
        TextPatternRange heading, TextPatternRange plan,
        TextPatternRange used, TextPatternRange reset)
    {
        var h = heading.GetBoundingRectangles();
        var p = plan.GetBoundingRectangles();
        var u = used.GetBoundingRectangles();
        var r = reset.GetBoundingRectangles();
        if (h.Length != 1 || p.Length != 1 || u.Length != 1 || r.Length != 1)
            return false;
        double hy = h[0].Top;
        double py = p[0].Top;
        // Layout-independent, generous relative vertical limits. This also
        // excludes text obscured by the settings modal or scrolled offscreen.
        return p[0].Width > 0 && u[0].Width > 0 && r[0].Width > 0
            && py >= hy - 20 && py <= hy + 260
            && Math.Abs(u[0].Top - py) <= 100
            && Math.Abs(r[0].Top - py) <= 140;
    }
}
