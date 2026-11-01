using System.Windows.Forms;

namespace MuseLoginShellPoc;

internal static class Program
{
    // Contains no credentials, user URLs, cookies or page data.
    private static readonly string DiagnosticPath = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
        "Solidaily", "MuseLoginShellPoc", "startup-diagnostic.txt");

    [STAThread]
    private static void Main(string[] args)
    {
        try
        {
            if (args.Contains("--passive-response-diagnostic", StringComparer.Ordinal))
            {
                ApplicationConfiguration.Initialize();
                using var diagnostic = new PassiveResponseDiagnostic();
                Application.Run(diagnostic);
                return;
            }
            if (args.Contains("--synthetic-webview2-self-test", StringComparer.Ordinal))
            {
                ApplicationConfiguration.Initialize();
                using var syntheticWeb = new SyntheticWebView2Window();
                Application.Run(syntheticWeb);
                return;
            }
            if (args.Contains("--synthetic-self-test", StringComparer.Ordinal))
            {
                ApplicationConfiguration.Initialize();
                using var synthetic = new SyntheticUsageWindow(runAutomaticSelfTest: true);
                Application.Run(synthetic);
                return;
            }
            Record("Process entered managed Main");
            ApplicationConfiguration.Initialize();
            Record("Windows Forms initialized");
            using var window = new LoginWindow();
            Record("Window constructed");
            window.Shown += (_, _) => Record("Window shown");
            Application.Run(window);
            Record("Message loop ended");
        }
        catch (Exception ex)
        {
            Record("Startup failed: " + ex.GetType().FullName);
            MessageBox.Show(
                "Muse検証版の起動に失敗しました。詳細は試作専用の起動診断ファイルを確認してください。",
                "Muse検証版 — 起動エラー", MessageBoxButtons.OK, MessageBoxIcon.Error);
        }
    }

    private static void Record(string message)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(DiagnosticPath)!);
            File.AppendAllText(DiagnosticPath,
                DateTimeOffset.Now.ToString("O") + " " + message + Environment.NewLine);
        }
        catch
        {
            // Diagnostics must not prevent startup.
        }
    }
}
