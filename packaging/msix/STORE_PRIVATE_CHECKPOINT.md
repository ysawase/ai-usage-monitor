# Microsoft Store Private Audience / self-only checkpoint

This checkpoint is for a Microsoft Store Private audience submission used only to obtain a Microsoft-signed package for SAC-on validation. It is not approval for public distribution.

## Current package model

- Package type: MSIX
- Architecture: x64
- Packaged executables:
  - claude-code-usage-monitor.exe
  - aum-quota.exe
- windows.startupTask: AIUsageMonitorStartup
- windows.appExecutionAlias: aum-quota.exe
- Restricted capability: runFullTrust
- Store build excludes self-update
- Microsoft Store signs the submitted MSIX after certification; no paid code-signing certificate is required for this Store-only path.

## Partner Center values required

Reserve the product as an MSIX or PWA app, then open:

Product management > Product identity

Copy these values exactly and case-sensitively:

1. Package/Identity/Name
2. Package/Identity/Publisher
3. Package/Properties/PublisherDisplayName

Build example:

    powershell -ExecutionPolicy Bypass -File .\packaging\msix\build-store-msix.ps1 -PackageIdentityName "<PackageIdentityName>" -Publisher "<Publisher>" -PublisherDisplayName "<PublisherDisplayName>"

The generated package is under target\msix-store.

## Private audience configuration

Use:

Pricing and availability > Visibility > Private audience

- Select only the personal Microsoft account used for this self-only validation.
- Do not configure public availability.
- Keep Public audience disabled after the checkpoint.

## Restricted capability justification draft

The package contains traditional Win32 desktop executables declared as win32App with mediumIL. The runFullTrust capability is required for this packaged desktop application model. The application uses standard desktop Win32 APIs for its taskbar/tray UI, launches installed provider CLI processes when needed, reads local app/provider configuration or sanitized cache data, and integrates with the packaged StartupTask and app execution alias. It does not request elevation and does not install a driver or Windows service.

## SAC-on self-only acceptance check

On a PC where Smart App Control remains On:

1. Sign in to Microsoft Store with the Private audience Microsoft account.
2. Open the Private audience listing.
3. Install the Microsoft-signed package from Store.
4. Confirm the app starts without SAC/reputation blocking.
5. Confirm widget/tray startup and exit.
6. Confirm Start with Windows through packaged StartupTask.
7. Confirm aum-quota.exe app execution alias.
8. Confirm configured provider acquisition without credential/raw-response exposure.
9. Confirm multi-monitor placement and cross-monitor drag.
10. Keep Public audience disabled.
