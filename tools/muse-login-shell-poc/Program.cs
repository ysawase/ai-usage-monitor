using System.Windows.Forms;

namespace MuseLoginShellPoc;

internal static class Program
{
    [STAThread]
    private static void Main()
    {
        ApplicationConfiguration.Initialize();
        Application.Run(new LoginWindow());
    }
}
