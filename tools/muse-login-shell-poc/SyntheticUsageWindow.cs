using System.Drawing;
using System.Windows.Automation;
using System.Windows.Forms;

namespace MuseLoginShellPoc;

/// <summary>
/// All content is hardcoded synthetic fixture data; never searches a Muse
/// window, WebView2, browser profile or other applications.
/// </summary>
internal sealed class SyntheticUsageWindow : Form
{
    private readonly Panel fixture = new()
    {
        Dock = DockStyle.Top,
        Height = 135,
        AccessibleName = "合成利用状況パネル",
        AccessibleDescription = "検証用の架空データ。Museアカウントとは無関係"
    };
    private readonly Label result = new()
    {
        Dock = DockStyle.Fill,
        TextAlign = ContentAlignment.TopLeft,
        Text = "検証用データのみ。下の読み取りボタンを押してください。"
    };

    internal SyntheticUsageWindow()
    {
        Text = "合成データ読み取り検証 — 実Museにはアクセスしません";
        Width = 630;
        Height = 330;
        var heading = new Label { Dock = DockStyle.Top, Height = 43, Text = "検証用（架空の利用率）", Font = new Font(Font, FontStyle.Bold) };
        var plan = new Label { Dock = DockStyle.Top, Height = 28, Text = "無料プラン" };
        var percent = new Label { Dock = DockStyle.Top, Height = 28, Text = "2%使用済み" };
        var reset = new Label { Dock = DockStyle.Top, Height = 28, Text = "1週間の上限は11月6日にリセットされます" };
        fixture.Controls.Add(reset);
        fixture.Controls.Add(percent);
        fixture.Controls.Add(plan);
        fixture.Controls.Add(heading);

        var read = new Button { Dock = DockStyle.Top, Height = 44, Text = "この合成パネルだけをUI Automationで読む" };
        read.Click += (_, _) => ProbeFixture();
        Controls.Add(result);
        Controls.Add(read);
        Controls.Add(fixture);
    }

    private void ProbeFixture()
    {
        try
        {
            // Scope is this exact panel's HWND, not RootElement / desktop.
            IntPtr handle = fixture.Handle;
            var root = AutomationElement.FromHandle(handle);
            var condition = new PropertyCondition(
                AutomationElement.ControlTypeProperty, ControlType.Text);
            var nodes = root.FindAll(TreeScope.Descendants, condition);
            var text = new List<string>();
            for (int i = 0; i < nodes.Count && i < 40; i++)
            {
                text.Add(nodes[i].Current.Name);
            }

            if (!SyntheticUsageParser.TryParse(text, out var reading) || reading is null)
            {
                result.Text = "FAIL：合成パネルの必要な表示文字列を取得・検証できませんでした。";
                return;
            }
            result.Text = $"PASS：合成パネルのみを読取。プラン={reading.Plan}、使用={reading.UsedPercent}%、残り={reading.RemainingPercent}%、リセット={reading.ResetMonth}月{reading.ResetDay}日。年・時刻は不明。実Museからの取得ではありません。";
        }
        catch (Exception)
        {
            result.Text = "FAIL：合成パネルのUI Automation読み取りに失敗しました。";
        }
    }
}
