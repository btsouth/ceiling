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

; Inno only removes {app} when its own bookkeeping says the directory is
; empty; this also covers a directory left behind after a file that was
; locked during uninstall.
[UninstallDelete]
Type: dirifempty; Name: "{app}"

[Code]
const
  // Must match the value rust/src/settings.rs writes for start at login.
  // The test in rust/src/settings/tests.rs asserts the two stay equal.
  StartAtLoginRunKey = 'Software\Microsoft\Windows\CurrentVersion\Run';
  StartAtLoginRunValue = 'Ceiling';

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

function NormalizedPath(Path: String): String;
begin
  Result := Trim(Path);
  StringChangeEx(Result, '/', '\', True);
  while (Length(Result) > 0) and (Result[Length(Result)] = '\') do
    Delete(Result, Length(Result), 1);
  Result := Lowercase(Result);
end;

// True for a binary this installation owns. Mirrors
// is_start_at_login_binary_name in rust/src/settings.rs, including the
// legacy codexbar-desktop.exe name an older build may have written to Run.
function IsInstalledCeilingBinary(Path: String): Boolean;
var
  Name: String;
begin
  Name := Lowercase(ExtractFileName(Path));
  Result :=
    ((Name = 'ceiling.exe') or (Name = 'codexbar-cli.exe') or (Name = 'codexbar-desktop.exe')) and
    (NormalizedPath(ExtractFileDir(Path)) = NormalizedPath(ExpandConstant('{app}')));
end;

// Split a Run command into its executable and arguments the same way
// parse_start_at_login_command in rust/src/settings.rs does: a quoted path,
// or an unquoted one ending at the first ".exe" followed by whitespace.
function SplitRunCommand(Command: String; var Executable, Arguments: String): Boolean;
var
  Lowered: String;
  Offset, Found, ExeEnd: Integer;
begin
  Result := False;
  Command := Trim(Command);
  if Command = '' then
    exit;

  if Command[1] = '"' then begin
    Delete(Command, 1, 1);
    Found := Pos('"', Command);
    if Found <= 1 then
      exit;
    Executable := Copy(Command, 1, Found - 1);
    Arguments := Copy(Command, Found + 1, Length(Command) - Found);
    Result := True;
    exit;
  end;

  Lowered := Lowercase(Command);
  Offset := 0;
  while True do begin
    Found := Pos('.exe', Copy(Lowered, Offset + 1, Length(Lowered) - Offset));
    if Found = 0 then
      exit;
    ExeEnd := Offset + Found + 3;
    if (ExeEnd = Length(Command)) or (Command[ExeEnd + 1] <= ' ') then begin
      Executable := Copy(Command, 1, ExeEnd);
      Arguments := Copy(Command, ExeEnd + 1, Length(Command) - ExeEnd);
      Result := True;
      exit;
    end;
    Offset := Offset + Found;
  end;
end;

// Ceiling writes Run\Ceiling as the bare quoted path of its executable.
// Anything else is left alone: a value pointing at another copy (a portable
// Ceiling, a different install directory, another app) or one the user has
// edited to add arguments no longer belongs to this installation.
function IsOwnedStartAtLoginCommand(Command: String): Boolean;
var
  Executable, Arguments: String;
begin
  Result :=
    SplitRunCommand(Command, Executable, Arguments) and
    (Trim(Arguments) = '') and
    IsInstalledCeilingBinary(Executable);
end;

procedure RemoveOwnedStartAtLoginValue();
var
  Command: String;
begin
  if not RegQueryStringValue(HKCU, StartAtLoginRunKey, StartAtLoginRunValue, Command) then
    exit;

  if not IsOwnedStartAtLoginCommand(Command) then begin
    Log('Keeping Run\' + StartAtLoginRunValue + ' because it does not belong to this installation: ' + Command);
    exit;
  end;

  if RegDeleteValue(HKCU, StartAtLoginRunKey, StartAtLoginRunValue) then
    Log('Removed Run\' + StartAtLoginRunValue + ': ' + Command)
  else
    Log('Could not remove Run\' + StartAtLoginRunValue + ': ' + Command);
end;

// Count this installation's running binaries, terminating each one when
// Terminate is set. A process started from another directory, such as a
// portable copy, is never touched.
function RunningInstalledBinaries(Service: Variant; Terminate: Boolean): Integer;
var
  Processes, Process: Variant;
  Index: Integer;
begin
  Result := 0;
  Processes := Service.ExecQuery(
    'SELECT ProcessId, ExecutablePath FROM Win32_Process ' +
    'WHERE Name = ''ceiling.exe'' OR Name = ''codexbar-cli.exe''');
  for Index := 0 to Processes.Count - 1 do begin
    Process := Processes.ItemIndex(Index);
    if VarIsNull(Process.ExecutablePath) or not IsInstalledCeilingBinary(Process.ExecutablePath) then
      continue;
    Result := Result + 1;
    if Terminate then begin
      Log('Stopping ' + Process.ExecutablePath + ' (PID ' + IntToStr(Process.ProcessId) + ')');
      try
        Process.Terminate();
      except
        // Already exiting; the wait below confirms it is gone.
        Log('Terminate failed: ' + GetExceptionMessage());
      end;
    end;
  end;
end;

// A running Ceiling holds ceiling.exe open, so the uninstaller could not
// delete it and the Run value kept launching it at the next sign-in.
procedure StopRunningCeiling();
var
  Locator, Service: Variant;
  Attempt: Integer;
begin
  try
    Locator := CreateOleObject('WbemScripting.SWbemLocator');
    Service := Locator.ConnectServer('.', 'root\CIMV2');
    if RunningInstalledBinaries(Service, True) = 0 then
      exit;
    for Attempt := 1 to 40 do begin
      Sleep(250);
      if RunningInstalledBinaries(Service, False) = 0 then
        exit;
    end;
    Log('Ceiling is still running; its files may not be removed.');
  except
    Log('Could not stop Ceiling: ' + GetExceptionMessage());
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then begin
    StopRunningCeiling();
    RemoveOwnedStartAtLoginValue();
  end;
end;
