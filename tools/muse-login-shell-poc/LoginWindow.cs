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
    private bool resetting;

    internal LoginWindow()
    {
        Text = "Muse ログイン検証（連携未完成）";
        Width = 950;
        Height = 720;
        MinimumSize = new Size(550, 400);

        var bar = new TableLayoutPanel { Dock = DockStyle.Top, Height = 64, ColumnCount = 3 };
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        bar.Controls.Add(open, 0, 0);
        bar.Controls.Add(erase, 1, 0);
        bar.Controls.Add(status, 2, 0);
        Controls.Add(browser);
        Controls.Add(bar);
        open.Click += (_, _) => browser.CoreWebView2?.Navigate(MuseUrl);
        erase.Click += async (_, _) => await ClearSessionAsync();
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
            var environment = await CoreWebView2Environment.CreateAsync(
                browserExecutableFolder: null, userDataFolder: directory);
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
        }
        catch (Exception)
        {
            status.Text = "WebView2を初期化できません。ランタイムの有無を確認してください。";
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
        open.Enabled = erase.Enabled = false;
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
            open.Enabled = erase.Enabled = true;
        }
    }
}
