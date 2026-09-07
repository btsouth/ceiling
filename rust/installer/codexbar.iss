#define MyAppName "Ceiling"
; Ceiling's AppUserModelID. Must match CEILING_AUMID in
; rust/src/notifications.rs and the identifier in tauri.conf.json.
#define AppUserModelId "io.github.tsouth89.ceiling"
#ifndef AppVersion
  #define AppVersion "0.0.0-dev"
#endif
#ifndef TargetBinDir
  #define TargetBinDir "..\\..\\target\\release"
#endif
#ifndef OutputDir
  #define OutputDir "..\\target\\installer"
#endif
#ifndef OutputBaseFilename
  #define OutputBaseFilename "Ceiling-" + AppVersion + "-Setup"
#endif
#ifndef VCRedistPath
  #define VCRedistPath "..\\target\\installer-deps\\vc_redist.x64.exe"
#endif
#ifndef WebView2InstallerPath
  #define WebView2InstallerPath "..\\target\\installer-deps\\MicrosoftEdgeWebview2Setup.exe"
#endif
#ifndef WebView2InstallerFileName
  #define WebView2InstallerFileName "MicrosoftEdgeWebview2Setup.exe"
#endif

[Setup]
; Literal on purpose. AppId is Inno's upgrade identity, deciding whether a
; new build replaces the installed one or lands beside it, so it is not
; worth routing through the preprocessor to save a repetition. The test in
; rust/src/notifications.rs asserts this and AppUserModelId stay equal.
AppId=io.github.tsouth89.ceiling
AppName={#MyAppName}
AppVersion={#AppVersion}
AppVerName={#MyAppName} {#AppVersion}
AppPublisher=Brandon South
AppPublisherURL=https://github.com/btsouth/ceiling
AppSupportURL=https://github.com/btsouth/ceiling/issues
AppUpdatesURL=https://github.com/btsouth/ceiling/releases
DefaultDirName={localappdata}\Programs\Ceiling
DefaultGroupName=Ceiling
DisableProgramGroupPage=yes
DisableDirPage=auto
DisableStartupPrompt=yes
PrivilegesRequired=lowest
UsePreviousAppDir=yes
CloseApplications=yes
WizardStyle=modern
Compression=lzma
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseFilename}
SetupIconFile=..\icons\icon.ico
UninstallDisplayIcon={app}\ceiling.exe
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; Flags: unchecked

[Files]
Source: "{#TargetBinDir}\ceiling.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#TargetBinDir}\codexbar-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\icons\icon.ico"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#VCRedistPath}"; Flags: dontcopy
Source: "{#WebView2InstallerPath}"; Flags: dontcopy

; AppUserModelID must match CEILING_AUMID in rust/src/notifications.rs. Windows
; resolves an app's notification identity from a Start Menu shortcut carrying
; this property, and keeps toasts in the notification center only for an app it
; can resolve. Without it here, every toast is treated as coming from an
; unregistered app: the banner shows, then Windows discards it.
[Icons]
Name: "{autoprograms}\Ceiling"; Filename: "{app}\ceiling.exe"; Parameters: "menubar"; WorkingDir: "{app}"; IconFilename: "{app}\icon.ico"; AppUserModelID: "{#AppUserModelId}"
Name: "{autodesktop}\Ceiling"; Filename: "{app}\ceiling.exe"; Parameters: "menubar"; WorkingDir: "{app}"; Tasks: desktopicon; IconFilename: "{app}\icon.ico"; AppUserModelID: "{#AppUserModelId}"

[Run]
Filename: "{app}\ceiling.exe"; Parameters: "menubar"; Description: "Launch Ceiling"; Flags: nowait postinstall skipifsilent; Check: CanLaunchCeiling

[Code]
var
  NeedsVCRedistRestart: Boolean;
  NeedsWebView2Restart: Boolean;

function WebView2InstalledInView(RootKey: Integer): Boolean;
var
  RuntimeVersion: String;
begin
  Result :=
    RegQueryStringValue(
      RootKey,
      'SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
      'pv',
      RuntimeVersion
    ) and
    (RuntimeVersion <> '');
end;

function WebView2NeedsInstall(): Boolean;
begin
  Result :=
    not WebView2InstalledInView(HKLM64) and
    not WebView2InstalledInView(HKLM32) and
    not WebView2InstalledInView(HKCU);
end;

procedure EnsureWebView2Installed();
var
  ResultCode: Integer;
begin
  if not WebView2NeedsInstall() then
    exit;

  ExtractTemporaryFile('{#WebView2InstallerFileName}');

  WizardForm.StatusLabel.Caption := 'Installing Microsoft Edge WebView2 Runtime...';
  WizardForm.ProgressGauge.Style := npbstMarquee;
  try
    if not Exec(
      ExpandConstant('{tmp}\{#WebView2InstallerFileName}'),
      '/silent /install',
      '',
      SW_HIDE,
      ewWaitUntilTerminated,
      ResultCode
    ) then
      RaiseException('Failed to start the Microsoft Edge WebView2 Runtime installer.');

    if (ResultCode <> 0) and (ResultCode <> 1638) and (ResultCode <> 3010) then
      RaiseException(
        'Microsoft Edge WebView2 Runtime installation failed with exit code ' +
        IntToStr(ResultCode) +
        '.'
      );

    if ResultCode = 3010 then
      NeedsWebView2Restart := True;
  finally
    WizardForm.ProgressGauge.Style := npbstNormal;
  end;
end;

function VCRedistInstalledInView(RootKey: Integer): Boolean;
var
  Installed: Cardinal;
begin
  Result :=
    RegQueryDWordValue(
      RootKey,
      'SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64',
      'Installed',
      Installed
    ) and
    (Installed = 1);
end;

function VCRedistNeedsInstall(): Boolean;
begin
  Result :=
    not VCRedistInstalledInView(HKLM64) and
    not VCRedistInstalledInView(HKLM32);
end;

procedure EnsureVCRedistInstalled();
var
  ResultCode: Integer;
begin
  if not VCRedistNeedsInstall() then
    exit;

  ExtractTemporaryFile('vc_redist.x64.exe');

  WizardForm.StatusLabel.Caption := 'Installing Microsoft Visual C++ Runtime...';
  WizardForm.ProgressGauge.Style := npbstMarquee;
  try
    if not Exec(
      ExpandConstant('{tmp}\vc_redist.x64.exe'),
      '/install /quiet /norestart',
      '',
      SW_HIDE,
      ewWaitUntilTerminated,
      ResultCode
    ) then
      RaiseException('Failed to start the Microsoft Visual C++ Runtime installer.');

    if (ResultCode <> 0) and (ResultCode <> 1638) and (ResultCode <> 3010) then
      RaiseException(
        'Microsoft Visual C++ Runtime installation failed with exit code ' +
        IntToStr(ResultCode) +
        '.'
      );

    if ResultCode = 3010 then
      NeedsVCRedistRestart := True;
  finally
    WizardForm.ProgressGauge.Style := npbstNormal;
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then begin
    EnsureWebView2Installed();
    EnsureVCRedistInstalled();
  end;
end;

function NeedRestart(): Boolean;
begin
  Result := NeedsVCRedistRestart or NeedsWebView2Restart;
end;

function GetCustomSetupExitCode(): Integer;
begin
  if NeedRestart() then
    Result := 3010
  else
    Result := 0;
end;

function CanLaunchCeiling(): Boolean;
begin
  Result := not NeedsVCRedistRestart and not NeedsWebView2Restart;
end;
