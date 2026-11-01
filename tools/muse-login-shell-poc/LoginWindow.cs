using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.WinForms;
using System.Drawing;
using System.Windows.Forms;

namespace MuseLoginShellPoc;

/// <summary>
/// Separate, opt-in login-only Windows experiment.
/// No quota fetching, Cookie APIs, DevTools, injection, or host messaging.
/// </summary>
internal sealed class LoginWindow : Form
{
    private const string MuseUrl = "https://muse.ai/";
    private readonly WebView2 browser = new() { Dock = DockStyle.Fill };
    private readonly Label status = new()
    {
        AutoSize = false,
        Dock = DockStyle.Fill,
        TextAlign = ContentAlignment.MiddleLeft,
        Text = "検証用：Museのログイン画面のみ表示。利用枠は取得しません。"
    };
    private readonly Button open = new() { Text = "Museを開く", AutoSize = true };
    private readonly Button erase = new() { Text = "この試作のログイン情報を消去", AutoSize = true };
    private readonly Button synthetic = new() { Text = "合成データ検証", AutoSize = true };
    private readonly Button readVisible = new() { Text = "表示中の利用状況を1回確認", AutoSize = true, Enabled = false };
    private bool resetting;

    internal LoginWindow()
    {
        Text = "Muse ログイン検証（連携未完成）";
        Width = 950;
        Height = 720;
        MinimumSize = new Size(550, 400);

        var bar = new TableLayoutPanel { Dock = DockStyle.Top, Height = 64, ColumnCount = 5 };
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        bar.Controls.Add(open, 0, 0);
        bar.Controls.Add(erase, 1, 0);
        bar.Controls.Add(synthetic, 2, 0);
        bar.Controls.Add(readVisible, 3, 0);
        bar.Controls.Add(status, 4, 0);
        Controls.Add(browser);
        Controls.Add(bar);
        open.Click += (_, _) => browser.CoreWebView2?.Navigate(MuseUrl);
        erase.Click += async (_, _) => await ClearSessionAsync();
        synthetic.Click += (_, _) => new SyntheticUsageWindow().Show(this);
        readVisible.Click += async (_, _) => await ReadVisibleUsageOnceAsync();
        Shown += async (_, _) => await InitializeBrowserAsync();
    }

    private async Task InitializeBrowserAsync()
    {
        open.Enabled = false;
        erase.Enabled = false;
        try
        {
            // A dedicated profile, not the user's Chrome / Opera / Edge profile.
            string directory = Path.Combine(
                Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
                "Solidaily", "MuseLoginShellPoc", "WebView2Profile");
            // Dedicated personal-only PoC: permit Windows UI Automation to
            // expose visible web content after the user's explicit read click.
            // This diagnostic flag is used only in this isolated experimental
            // profile. It must NOT be carried into the public Store product
            // without a separate provider-policy and performance review.
            var options = new CoreWebView2EnvironmentOptions
            {
                AdditionalBrowserArguments = "--force-renderer-accessibility"
            };
            var environment = await CoreWebView2Environment.CreateAsync(
                browserExecutableFolder: null, userDataFolder: directory,
                options: options);
            await browser.EnsureCoreWebView2Async(environment);
            var core = browser.CoreWebView2;
            core.Settings.IsWebMessageEnabled = false;
            core.Settings.AreDevToolsEnabled = false;
            core.Settings.AreHostObjectsAllowed = false;
            core.Settings.IsStatusBarEnabled = false;
            core.Settings.AreDefaultContextMenusEnabled = false;
            core.NavigationStarting += (_, e) =>
            {
                if (!MuseOriginPolicy.IsAllowed(e.Uri))
                {
                    e.Cancel = true;
                    // Display only the hostname; never display or log authentication paths or query values.
                    string hostname = "確認できない接続先";
                    if (Uri.TryCreate(e.Uri, UriKind.Absolute, out var target) &&
                        (target.Scheme == Uri.UriSchemeHttps || target.Scheme == Uri.UriSchemeHttp) &&
                        target.IdnHost.Length is > 0 and <= 120)
                    {
                        hostname = target.IdnHost;
                    }
                    status.Text = "許可されていない接続先を停止: " + hostname;
                }
            };
            core.NewWindowRequested += (_, e) =>
            {
                // Deny popup windows, including automatic new-tab flows.
                e.Handled = true;
                status.Text = "別ウィンドウは開きません。必要な場合はこの試作を閉じて通常のブラウザで操作してください。";
            };
            core.PermissionRequested += (_, e) =>
            {
                e.State = CoreWebView2PermissionState.Deny;
            };
            core.Navigate(MuseUrl);
            status.Text = "Muse公式画面にログインできます。試作は利用率を読み取りません。";
            open.Enabled = true;
            erase.Enabled = true;
            readVisible.Enabled = true;
        }
        catch (Exception)
        {
            status.Text = "WebView2を初期化できません。ランタイムの有無を確認してください。";
        }
    }


    private async Task ReadVisibleUsageOnceAsync()
    {
        if (resetting || browser.CoreWebView2 is null || !readVisible.Enabled)
            return;
        // No background polling, navigation, HTTP calls, DOM injection or
        // browser-profile access. Require the user to open the official
        // Settings > General > Usage view before clicking this button.
        var current = browser.CoreWebView2.Source;
        if (!Uri.TryCreate(current, UriKind.Absolute, out var url)
            || url.Scheme != Uri.UriSchemeHttps
            || !url.Host.Equals("muse.ai", StringComparison.OrdinalIgnoreCase))
        {
            status.Text = "取得不可：muse.ai公式画面を開いてください。";
            return;
        }
        readVisible.Enabled = false;
        status.Text = "表示中の利用状況を確認中（保存・送信なし）";
        try
        {
            IntPtr handle = Handle;
            var probeTask = Task.Run(() => MuseVisibleUsageReader.ReadOnce(handle));
            if (await Task.WhenAny(probeTask, Task.Delay(TimeSpan.FromSeconds(12))) != probeTask)
            {
                status.Text = "取得不可：時間切れです。";
                return;
            }
            var result = await probeTask;
            if (result.Status != MuseVisibleProbeStatus.Success || result.Reading is null)
            {
                status.Text = "取得不可：" + result.Status
                    + "。設定 → 一般 → 利用状況を表示してください。";
                return;
            }
            var reading = result.Reading;
            status.Text = $"検証用表示：{reading.Plan} 使用{reading.UsedPercent}%"
                + $" / 残り{reading.RemainingPercent}%"
                + $" / {reading.ResetMonth}月{reading.ResetDay}日リセット（年・時刻不明）";
        }
        catch (Exception)
        {
            status.Text = "取得不可：読み取りに失敗しました。";
        }
        finally
        {
            readVisible.Enabled = !resetting;
        }
    }

    private async Task ClearSessionAsync()
    {
        if (browser.CoreWebView2 is null || resetting) return;
        if (MessageBox.Show(this,
                "この試作専用のCookie・履歴などを消去します。Museアカウント自体は削除しません。続けますか？",
                "試作のログイン情報を消去",
                MessageBoxButtons.YesNo, MessageBoxIcon.Question) != DialogResult.Yes) return;
        resetting = true;
        open.Enabled = erase.Enabled = readVisible.Enabled = false;
        try
        {
            // Leave the live page first; this profile alone is affected.
            browser.CoreWebView2.Navigate("about:blank");
            await browser.CoreWebView2.Profile.ClearBrowsingDataAsync(
                CoreWebView2BrowsingDataKinds.AllProfile);
            status.Text = "この試作のログイン情報を消去しました。";
            browser.CoreWebView2.Navigate(MuseUrl);
        }
        catch (Exception)
        {
            status.Text = "ログイン情報の消去に失敗しました。成功扱いにはしません。";
        }
        finally
        {
            resetting = false;
            open.Enabled = erase.Enabled = readVisible.Enabled = true;
        }
    }
}
