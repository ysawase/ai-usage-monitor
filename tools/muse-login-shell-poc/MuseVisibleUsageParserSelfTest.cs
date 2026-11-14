namespace MuseLoginShellPoc;

// Synthetic strings only; never accesses a Muse session, DOM, cookies or network.
internal static class MuseVisibleUsageParserSelfTest
{
    internal static bool Run()
    {
        const string canonical = "設定 一般 利用状況 無料プラン 2%使用済み 1週間の上限は11月20日にリセットされます";
        if (!ExpectSuccess(canonical, 2, 11, 20))
            return false;
        if (!ExpectSuccess("利用状況 無料プラン 0%使用済み 1週間の上限は11月20日にリセットされます", 0, 11, 20))
            return false;
        // Format marks inside the tier name: common accessibility-text artifact.
        if (!ExpectSuccess("利用状況 無\u200b料\u200eプ\u2060ラン 2％使用済み 1週間の上限は11月20日にリセットされます", 2, 11, 20))
            return false;
        // Fullwidth letters/digits and percent are normalized by FormKC.
        if (!ExpectSuccess("利用状況 無料プラン ２％使用済み １週間の上限は１１月２０日にリセットされます", 2, 11, 20))
            return false;
        // Accessibility providers can expose a compact visible card before its heading.
        if (!ExpectNearSuccess(
            "無料プラン 4%使用済み 1週間の上限は11月20日にリセットされます 利用状況", 4))
            return false;
        if (!ExpectNearFailure(
            "無料プラン 4%使用済み 1週間の上限は11月20日にリセットされます 利用状況 6%使用済み"))
            return false;
        if (!ExpectNearFailure(
            "無料プラン 4%使用済み 1週間の上限は11月20日にリセットされます "
            + new string('X', 340) + " 利用状況"))
            return false;
        if (!ExpectFailure("チャット: 無料プラン 2%使用済み 1週間の上限は11月20日にリセットされます",
            MuseUsageParseStatus.MissingUsageSection))
            return false;
        if (!ExpectFailure("利用状況 2%使用済み 1週間の上限は11月20日にリセットされます",
            MuseUsageParseStatus.MissingPlan))
            return false;
        if (!ExpectFailure("利用状況 無料プラン 2%使用済み 4%使用済み 1週間の上限は11月20日にリセットされます",
            MuseUsageParseStatus.AmbiguousOrInvalid))
            return false;
        if (!ExpectFailure("利用状況 無料プラン 2%使用済み 1週間の上限は11月20日にリセットされます 無料プラン",
            MuseUsageParseStatus.AmbiguousOrInvalid))
            return false;
        return true;
    }

    private static bool ExpectNearSuccess(string text, int used)
    {
        return MuseVisibleUsageParser.TryParseNearHeading(text, out var reading, out var status)
            && status == MuseUsageParseStatus.Success
            && reading is { Plan: "無料プラン", ResetMonth: 11, ResetDay: 20 }
            && reading.UsedPercent == used;
    }

    private static bool ExpectNearFailure(string text)
    {
        return !MuseVisibleUsageParser.TryParseNearHeading(text, out var reading, out _)
            && reading is null;
    }

    private static bool ExpectSuccess(string text, int used, int month, int day)
    {
        return MuseVisibleUsageParser.TryParse(text, out var reading, out var state)
            && state == MuseUsageParseStatus.Success
            && reading is { Plan: "無料プラン" }
            && reading.UsedPercent == used
            && reading.RemainingPercent == 100 - used
            && reading.ResetMonth == month
            && reading.ResetDay == day;
    }

    private static bool ExpectFailure(string text, MuseUsageParseStatus status)
    {
        return !MuseVisibleUsageParser.TryParse(text, out var reading, out var state)
            && reading is null && state == status;
    }
}
