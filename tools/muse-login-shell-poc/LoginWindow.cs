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
    private readonly Button captureQuota = new() { Text = "設定画面の応答を取得（1回）", AutoSize = true, Enabled = false };
    private readonly System.Windows.Forms.Timer captureTimer = new() { Interval = 60_000 };
    private bool captureArmed;
    private bool captureReading;
    private int captureCandidates;
    private bool resetting;

    internal LoginWindow()
    {
        Text = "Muse ログイン検証（連携未完成）";
        Width = 950;
        Height = 720;
        MinimumSize = new Size(550, 400);

        var bar = new TableLayoutPanel { Dock = DockStyle.Top, Height = 64, ColumnCount = 6 };
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.AutoSize));
        bar.ColumnStyles.Add(new ColumnStyle(SizeType.Percent, 100));
        bar.Controls.Add(open, 0, 0);
        bar.Controls.Add(erase, 1, 0);
        bar.Controls.Add(synthetic, 2, 0);
        bar.Controls.Add(readVisible, 3, 0);
        bar.Controls.Add(captureQuota, 4, 0);
        bar.Controls.Add(status, 5, 0);
        Controls.Add(browser);
        Controls.Add(bar);
        open.Click += (_, _) => browser.CoreWebView2?.Navigate(MuseUrl);
        erase.Click += async (_, _) => await ClearSessionAsync();
        synthetic.Click += (_, _) => new SyntheticUsageWindow().Show(this);
        readVisible.Click += async (_, _) => await ReadVisibleUsageOnceAsync();
        captureQuota.Click += (_, _) => ArmQuotaCapture();
        captureTimer.Tick += (_, _) => {
            if (captureArmed) StopQuotaCapture("Muse利用枠の応答が届きませんでした。取得不可として扱います。");
        };
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
            // Dormant unless the user explicitly arms one limited response read.
            // No browser cookie APIs, page script injection or new HTTP requests.
            core.WebResourceResponseReceived += ObserveMuseQuotaResponse;
            core.Navigate(MuseUrl);
            status.Text = "Muse専用画面。利用枠の応答検証はボタンを押した時だけ有効です。";
            open.Enabled = true;
            erase.Enabled = true;
            readVisible.Enabled = true;
            captureQuota.Enabled = true;
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
                    + (result.ParseStatus is null ? "" : " / " + result.ParseStatus)
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

    private void ArmQuotaCapture()
    {
        if (resetting || browser.CoreWebView2 is null || captureReading) return;
        var url = browser.CoreWebView2.Source;
        if (!Uri.TryCreate(url, UriKind.Absolute, out var current)
            || current.Scheme != Uri.UriSchemeHttps
            || !current.Host.Equals("muse.ai", StringComparison.OrdinalIgnoreCase))
        {
            status.Text = "muse.ai公式画面でのみ応答を確認できます。";
            return;
        }
        // Explicit, single-use opt-in. The user opens Settings > General > Usage.
        // Nothing is sent by this code: the embedded browser makes its own requests.
        captureCandidates = 0;
        captureArmed = true;
        captureTimer.Stop();
        captureTimer.Start();
        captureQuota.Enabled = false;
        status.Text = "60秒以内にMuseの「設定 → 利用状況」を開いてください。応答は保存・送信しません。";
    }

    private void StopQuotaCapture(string message)
    {
        captureArmed = false;
        captureTimer.Stop();
        if (!IsDisposed)
        {
            captureQuota.Enabled = !resetting && browser.CoreWebView2 is not null;
            status.Text = message;
        }
    }

    private async void ObserveMuseQuotaResponse(
        object? sender, CoreWebView2WebResourceResponseReceivedEventArgs e)
    {
        if (!captureArmed || captureReading || resetting || IsDisposed)
            return;
        // Only the exact first-party muse.ai origin. Never read auth.muse.ai,
        // unrelated third-party resources, GET responses or the user's cookies.
        if (!Uri.TryCreate(e.Request.Uri, UriKind.Absolute, out var uri)
            || uri.Scheme != Uri.UriSchemeHttps
            || !uri.Host.Equals("muse.ai", StringComparison.OrdinalIgnoreCase)
            || uri.Port != 443
            || !e.Request.Method.Equals("POST", StringComparison.OrdinalIgnoreCase)
            || !e.Request.Headers.Contains("Next-Action"))
            return;
        if (e.Response.StatusCode != 200
            || !e.Response.Headers.Contains("Content-Type")
            || !e.Response.Headers.GetHeader("Content-Type").StartsWith(
                "text/x-component", StringComparison.OrdinalIgnoreCase))
            return;

        captureReading = true;
        try
        {
            var body = await e.Response.GetContentAsync();
            if (body is null)
            {
                StopQuotaCapture("応答本文がありません。");
                return;
            }
            // WebView2 recommends reading its stream on a worker thread.
            // Bound memory to 64 KiB and never write the raw body to disk/log/clipboard.
            byte[] bytes = await Task.Run(() =>
            {
                using (body)
                using (var buffer = new MemoryStream())
                {
                    var part = new byte[2048];
                    while (true)
                    {
                        int n = body.Read(part, 0, part.Length);
                        if (n == 0) break;
                        if (buffer.Length + n > MusePassiveQuotaParser.MaxBytes)
                            throw new InvalidDataException("Response too large");
                        buffer.Write(part, 0, n);
                    }
                    return buffer.ToArray();
                }
            });
            var state = MusePassiveQuotaParser.Parse(
                System.Text.Encoding.UTF8.GetString(bytes), out var quota);
            if (state == MusePassiveQuotaParseState.Success && quota is not null)
            {
                var reset = quota.WeeklyResetAt is null
                    ? "リセット日時不明"
                    : TimeZoneInfo.ConvertTimeBySystemTimeZoneId(
                        quota.WeeklyResetAt.Value, "Tokyo Standard Time")
                        .ToString("yyyy/MM/dd HH:mm") + " JST";
                StopQuotaCapture(
                    $"検証成功（非保存）：{quota.Plan} / 週間使用 {quota.UsedPercent:0.##}% / {reset}");
            }
            else if (state != MusePassiveQuotaParseState.NoQuota)
            {
                StopQuotaCapture("取得不可：応答形式が不明・不正、または長すぎます。");
            }
            else if (++captureCandidates >= 6)
            {
                StopQuotaCapture("利用枠の応答が確認できません。別の応答を繰り返し読みません。");
            }
        }
        catch (Exception)
        {
            // Exception details, HTTP headers, URLs and response contents may
            // contain private data; never stringify or persist them.
            StopQuotaCapture("応答取得に失敗しました（認証・通信・仕様変更など原因未確定）。");
        }
        finally
        {
            captureReading = false;
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
        captureArmed = false;
        captureTimer.Stop();
        open.Enabled = erase.Enabled = readVisible.Enabled = captureQuota.Enabled = false;
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
            open.Enabled = erase.Enabled = readVisible.Enabled = captureQuota.Enabled = true;
        }
    }
}
