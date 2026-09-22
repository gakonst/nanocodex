#ifndef AppVersion
  #define AppVersion "dev"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0.0"
#endif

#define AppName "Nanocodex Hand"
#define AppPublisher "Nanocodex"

[Setup]
AppId={{4B87C5CE-A499-4CB9-AE58-11E7BB87C79C}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL=https://github.com/gakonst/nanocodex
AppSupportURL=https://github.com/gakonst/nanocodex/issues
DefaultDirName={autopf}\Nanocodex Hand
UsePreviousAppDir=no
DefaultGroupName=Nanocodex Hand
DisableProgramGroupPage=yes
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
OutputDir=..\..\dist\windows-hand
OutputBaseFilename=nanocodex-hand-setup-x86_64
UninstallDisplayName=Nanocodex Hand
VersionInfoVersion={#NumericVersion}
VersionInfoCompany={#AppPublisher}
VersionInfoDescription=Nanocodex Windows Hand installer
VersionInfoProductName={#AppName}
VersionInfoProductVersion={#NumericVersion}
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "payload\nanocodex2.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "payload\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "payload\ffmpeg-*.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "run-hand.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "setup-hand.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "setup-service.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "hand-service.cs"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Start or repair Nanocodex Hand"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\setup-hand.ps1"" -Action Repair -Service -InstallDir ""{app}"""; WorkingDir: "{app}"
Name: "{group}\Stop Nanocodex Hand"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\setup-hand.ps1"" -Action Stop -Service -InstallDir ""{app}"""; WorkingDir: "{app}"
Name: "{group}\Nanocodex Hand logs"; Filename: "{localappdata}\Nanocodex\Hand"
Name: "{group}\Uninstall Nanocodex Hand"; Filename: "{uninstallexe}"

[Run]
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ""{app}\setup-hand.ps1"" -Action Repair -InstallDir ""{app}"" -Service -SkipLogin"; Flags: runhidden waituntilterminated runascurrentuser; Check: HasConfiguredHand
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoLogo -NoProfile -ExecutionPolicy Bypass -File ""{app}\setup-hand.ps1"" -Action Install -InstallDir ""{app}"" -Service"; Description: "Sign in and connect this computer now"; Flags: postinstall waituntilterminated skipifsilent runasoriginaluser

[UninstallRun]
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File ""{app}\setup-hand.ps1"" -Action Uninstall -Service -InstallDir ""{app}"""; Flags: runhidden waituntilterminated; RunOnceId: "RemoveNanocodexHandTask"

[UninstallDelete]
Type: files; Name: "{app}\nanocodex-hand-service.exe"
Type: files; Name: "{app}\service.log"
Type: files; Name: "{app}\service.log.1"

[Messages]
WelcomeLabel2=This installs Nanocodex Hand and its background Windows service.%n%nAfter installation, sign in with a phone number and the six-digit SMS code. The service starts at boot and keeps the Hand running whenever this Windows user is signed in. The connected Nanocodex account can control this user's apps and files. Desktop control requires a signed-in Windows session.

[Code]
function HasConfiguredHand: Boolean;
begin
  Result := FileExists(ExpandConstant('{app}\hand-service.xml'));
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
  SetupScript: String;
  Arguments: String;
begin
  Result := '';
  SetupScript := ExpandConstant('{app}\setup-hand.ps1');
  if FileExists(SetupScript) then
  begin
    Arguments := '-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File "' +
      SetupScript + '" -Action Stop -Service -InstallDir "' + ExpandConstant('{app}') + '"';
    if (not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
      Arguments, '', SW_HIDE, ewWaitUntilTerminated, ResultCode)) or
      (ResultCode <> 0) then
      Result := 'Could not stop the existing Nanocodex Hand. Close it and try again.';
  end;
end;
