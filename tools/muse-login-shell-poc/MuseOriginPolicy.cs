using System;

namespace MuseLoginShellPoc;

/// <summary>
/// Exact, HTTPS-only navigation policy for an isolated Muse sign-in shell.
/// This is deliberately narrower than all of *.muse.ai.
/// </summary>
public static class MuseOriginPolicy
{
    public static bool IsAllowed(string? destination)
    {
        if (string.IsNullOrWhiteSpace(destination) ||
            !Uri.TryCreate(destination, UriKind.Absolute, out var url))
        {
            return false;
        }

        // The blank page is only for resetting the dedicated local profile.
        if (url.IsAbsoluteUri && url.Scheme == "about" && url.OriginalString == "about:blank")
        {
            return true;
        }

        return url.Scheme == Uri.UriSchemeHttps &&
               url.Port == 443 &&
               string.IsNullOrEmpty(url.UserInfo) &&
               (url.Host.Equals("muse.ai", StringComparison.OrdinalIgnoreCase) ||
                url.Host.Equals("auth.muse.ai", StringComparison.OrdinalIgnoreCase) ||
                // The Muse login PoC attempted a redirect to the exact Facebook login host.
                // Never allow *.facebook.com or accept non-HTTPS origins.
                url.Host.Equals("www.facebook.com", StringComparison.OrdinalIgnoreCase) ||
                // Observed second blocked origin on the same Muse login flow.
                // Only this first-party Instagram hostname is permitted.
                url.Host.Equals("www.instagram.com", StringComparison.OrdinalIgnoreCase));
    }
}
