using System.Text;
using System.Text.Json;

namespace MuseLoginShellPoc;

/// <summary>
/// Metadata-only progress receipt for a user-opted-in local experiment.
/// Never records quota values, user identity, cookies, URLs, HTTP headers,
/// raw responses or exception messages. Accessible only as an ordinary
/// local-user file to an already-authorized read-only diagnostic connector.
/// </summary>
internal sealed class MuseRefreshHealth
{
    private readonly string statusPath;
    private bool started;
    private string state = "stopped";
    private string lastOutcome = "none";
    private DateTimeOffset? sessionStart;
    private DateTimeOffset? lastAttempt;
    private DateTimeOffset? previousSuccess;
    private DateTimeOffset? lastSuccess;
    private int successCount;

    internal MuseRefreshHealth(string? dataDirectory = null)
    {
        var directory = dataDirectory ?? Path.Combine(
            Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData),
            "Solidaily", "MuseLoginShellPoc");
        statusPath = Path.Combine(directory, "refresh-health.json");
    }

    internal string StatusPath => statusPath;

    internal void Start(DateTimeOffset utcNow)
    {
        started = true;
        state = "running";
        lastOutcome = "not_checked";
        sessionStart = utcNow.ToUniversalTime();
        lastAttempt = null;
        previousSuccess = null;
        lastSuccess = null;
        successCount = 0;
        Write(utcNow);
    }

    internal void Attempt(DateTimeOffset utcNow)
    {
        if (!started || state != "running") return;
        lastAttempt = utcNow.ToUniversalTime();
        lastOutcome = "checking";
        Write(utcNow);
    }

    internal void Success(DateTimeOffset utcNow)
    {
        if (!started || state != "running") return;
        previousSuccess = lastSuccess;
        lastSuccess = utcNow.ToUniversalTime();
        successCount++;
        lastOutcome = "success";
        Write(utcNow);
    }

    internal void Failure(DateTimeOffset utcNow, string outcome)
    {
        if (!started || state != "running") return;
        // The caller must supply an allowlisted internal status label.
        lastOutcome = outcome switch
        {
            "NoSession" => "no_session",
            "AuthenticationRequired" => "authentication_required",
            "RateLimited" => "rate_limited",
            "ActionChanged" => "action_changed",
            "InvalidResponse" => "invalid_response",
            "NetworkFailure" => "network_failure",
            "TimedOut" => "timed_out",
            _ => "unknown_failure"
        };
        Write(utcNow);
    }

    internal void Stop(DateTimeOffset utcNow)
    {
        if (!started) return;
        state = "stopped";
        if (lastOutcome is "checking" or "not_checked")
            lastOutcome = "cancelled";
        Write(utcNow);
        started = false;
    }

    private void Write(DateTimeOffset utcNow)
    {
        string? tempPath = null;
        try
        {
            var dir = Path.GetDirectoryName(statusPath)!;
            Directory.CreateDirectory(dir);
            // Keep the schema flat and explicitly allowlisted. No general
            // object serialization, and no logging of exception.ToString().
            var data = new
            {
                schema_version = 1,
                state,
                session_started_utc = sessionStart,
                last_attempt_utc = lastAttempt,
                previous_success_utc = previousSuccess,
                last_success_utc = lastSuccess,
                success_count_in_session = successCount,
                last_outcome = lastOutcome,
                recorded_utc = utcNow.ToUniversalTime()
            };
            tempPath = statusPath + "." + Guid.NewGuid().ToString("N") + ".tmp";
            File.WriteAllText(tempPath, JsonSerializer.Serialize(data),
                new UTF8Encoding(encoderShouldEmitUTF8Identifier: false));
            File.Move(tempPath, statusPath, overwrite: true);
            tempPath = null;
        }
        catch (Exception)
        {
            // Diagnostics may be unavailable due to file permissions or disk
            // errors, but may never interrupt an otherwise valid quota read.
        }
        finally
        {
            if (tempPath is not null)
            {
                try { File.Delete(tempPath); } catch { }
            }
        }
    }
}
