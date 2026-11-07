using System.Drawing;
using System.Windows.Automation;
using System.Windows.Forms;

namespace MuseLoginShellPoc;

/// <summary>
/// Entirely synthetic controls: never inspect Muse WebView2, other windows,
/// a user's browser, authentication state, cookies or conversation contents.
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
        Text = "検証用データのみ。読み取りボタンを押してください。"
    };
    private bool checking;

    internal SyntheticUsageWindow(bool runAutomaticSelfTest = false)
    {
        Text = "合成データ読み取り検証 — 実Museにはアクセスしません";
        Width = 630;
        Height = 330;

        var heading = new Label
        {
            Dock = DockStyle.Top,
            Height = 43,
            Text = "検証用（架空の利用率）",
            Font = new Font(Font, FontStyle.Bold)
        };
        var plan = new Label { Dock = DockStyle.Top, Height = 28, Text = "無料プラン" };
        var percent = new Label { Dock = DockStyle.Top, Height = 28, Text = "2%使用済み" };
        var reset = new Label
        {
            Dock = DockStyle.Top,
            Height = 28,
            Text = "1週間の上限は11月6日にリセットされます"
        };
        fixture.Controls.Add(reset);
        fixture.Controls.Add(percent);
        fixture.Controls.Add(plan);
        fixture.Controls.Add(heading);

        var read = new Button
        {
            Dock = DockStyle.Top,
            Height = 44,
            Text = "この合成パネルだけをUI Automationで読む"
        };
        read.Click += async (_, _) => await ProbeFixtureAsync();

        Controls.Add(result);
        Controls.Add(read);
        Controls.Add(fixture);

        if (runAutomaticSelfTest)
        {
            Shown += async (_, _) =>
            {
                bool ok = await ProbeFixtureAsync();
                Environment.ExitCode = ok ? 0 : 1;
                Close();
            };
        }
    }

    private async Task<bool> ProbeFixtureAsync()
    {
        if (checking) return false;
        checking = true;
        try
        {
            // Obtain this dedicated synthetic panel's HWND on the UI thread.
            // Background work must never search AutomationElement.RootElement.
            IntPtr panelHandle = fixture.Handle;
            Task<string[]> readTask = Task.Run(() =>
            {
                var root = AutomationElement.FromHandle(panelHandle);
                var condition = new PropertyCondition(
                    AutomationElement.ControlTypeProperty, ControlType.Text);
                var nodes = root.FindAll(TreeScope.Descendants, condition);
                var text = new List<string>(Math.Min(nodes.Count, 40));
                for (int i = 0; i < nodes.Count && i < 40; i++)
                    text.Add(nodes[i].Current.Name);
                return text.ToArray();
            });

            // If UIA stalls, fail the experiment without blocking the UI.
            if (await Task.WhenAny(readTask, Task.Delay(TimeSpan.FromSeconds(12))) != readTask)
            {
                result.Text = "FAIL：合成パネルのUI Automationが時間切れです。";
                return false;
            }

            string[] labels = await readTask;
            if (!SyntheticUsageParser.TryParse(labels, out var reading) || reading is null)
            {
                result.Text = "FAIL：合成パネルの必要な表示文字列を取得・検証できませんでした。";
                return false;
            }

            result.Text =
                $"PASS：合成パネルのみを読取。プラン={reading.Plan}、使用={reading.UsedPercent}%、残り={reading.RemainingPercent}%、リセット={reading.ResetMonth}月{reading.ResetDay}日。年・時刻は不明。実Museからの取得ではありません。";
            return true;
        }
        catch (Exception)
        {
            result.Text = "FAIL：合成パネルのUI Automation読み取りに失敗しました。";
            return false;
        }
        finally
        {
            checking = false;
        }
    }
}
