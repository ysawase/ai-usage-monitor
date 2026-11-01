using Microsoft.Web.WebView2.Core;
using Microsoft.Web.WebView2.WinForms;
using System.Drawing;
using System.Windows.Forms;

namespace MuseLoginShellPoc;

/// <summary>
/// Synthetic HTML rendered in a fresh, isolated WebView2. Does not navigate
/// to Muse or read the real Muse session, browser profile, chat, or cookies.
/// </summary>
internal sealed class SyntheticWebView2Window : Form
{
    private const string FixtureHtml = """
        <!doctype html>
        <html lang="ja">
        <head><meta charset="utf-8"><title>架空の利用状況</title></head>
        <body>
          <main aria-label="架空の利用状況">
            <h1>利用状況</h1>
            <section aria-label="週間利用枠">
              <div>無料プラン</div>
              <div>2%使用済み</div>
              <div>1週間の上限は11月6日にリセットされます</div>
            </section>
          </main>
        </body>
        </html>
        """;

    private readonly WebView2 fixtureBrowser = new() { Dock = DockStyle.Fill };
    private readonly Label verdict = new()
    {
        Dock = DockStyle.Top,
        Height = 40,
        TextAlign = ContentAlignment.MiddleLeft,
        Text = "合成HTMLを検証しています（Museへの接続なし）"
    };
    private readonly System.Windows.Forms.Timer timeout = new() { Interval = 20000 };
    private readonly string profileDir = Path.Combine(
        Path.GetTempPath(), "MuseSyntheticWebView2-" + Guid.NewGuid().ToString("N"));
    private bool finished;
    private static readonly string DiagnosticFile = Path.Combine(
        Path.GetTempPath(), "MuseSyntheticWebView2-UiaResult.txt");
    private static void Diagnose(string category)
    {
        try { File.WriteAllText(DiagnosticFile, category); } catch { }
    }

    internal SyntheticWebView2Window()
    {
        Text = "合成WebView2読み取り検証 — Museとは無関係";
        Width = 720;
        Height = 430;
        Controls.Add(fixtureBrowser);
        Controls.Add(verdict);
        Shown += async (_, _) => await StartAsync();
        timeout.Tick += (_, _) => { Diagnose("timeout"); Complete(false); };
        timeout.Start();
    }

    private async Task StartAsync()
    {
        try
        {
            // Test fixture only. Microsoft documents this flag for development
            // diagnostics, not production usage. The real Muse WebView2 never
            // receives any such browser flags.
            var options = new CoreWebView2EnvironmentOptions
            {
                AdditionalBrowserArguments = "--force-renderer-accessibility"
            };
            var environment = await CoreWebView2Environment.CreateAsync(
                userDataFolder: profileDir, options: options);
            await fixtureBrowser.EnsureCoreWebView2Async(environment);
            fixtureBrowser.CoreWebView2.Settings.IsWebMessageEnabled = false;
            fixtureBrowser.CoreWebView2.Settings.AreDevToolsEnabled = false;
            fixtureBrowser.CoreWebView2.Settings.AreHostObjectsAllowed = false;
            fixtureBrowser.CoreWebView2.NavigationCompleted += async (_, navigation) =>
            {
                if (!navigation.IsSuccess)
                {
                    Diagnose("navigation_failed");
                    Complete(false);
                    return;
                }
                try
                {
                    // Allow the browser accessibility tree to settle. Read
                    // only this entirely synthetic test window's HWND (including WebView2).
                    await Task.Delay(400);
                    // Run the EXACT reader and parser used by the real
                    // user-click-only PoC; no independent synthetic shortcut.
                    // This window contains only synthetic HTML in a fresh
                    // profile, and is never connected to Muse.
                    IntPtr controlHandle = Handle;
                    var read = await Task.Run(
                        () => MuseVisibleUsageReader.ReadOnce(controlHandle));
                    Diagnose("real_reader_status=" + read.Status);
                    bool pass = read.Status == MuseVisibleProbeStatus.Success
                        && read.Reading is { Plan: "無料プラン", UsedPercent: 2,
                            RemainingPercent: 98, ResetMonth: 11, ResetDay: 6 };
                    Complete(pass);
                }
                catch (Exception ex)
                {
                    Diagnose("uia_exception=" + ex.GetType().Name);
                    Complete(false);
                }
            };

            // about:blank document from an in-memory synthetic fixture.
            // This never touches an authenticated Muse origin.
            fixtureBrowser.NavigateToString(FixtureHtml);
        }
        catch (Exception ex)
        {
            Diagnose("webview_initialization_exception=" + ex.GetType().Name);
            Complete(false);
        }
    }

    private void Complete(bool passed)
    {
        if (finished) return;
        finished = true;
        timeout.Stop();
        verdict.Text = passed
            ? "PASS: 合成WebView2の使用率2%、残り98%、リセット11月6日"
            : "FAIL: 合成WebView2のアクセシビリティ読取が未成立";
        Environment.ExitCode = passed ? 0 : 1;
        Close();
    }
}
