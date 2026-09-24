#ifndef AppVersion
  #define AppVersion "dev"
#endif
#ifndef NumericVersion
  #define NumericVersion "0.0.0.0"
#endif

#define AppName "Nanocodex"
#define AppPublisher "Nanocodex"

[Setup]
AppId={{4B87C5CE-A499-4CB9-AE58-11E7BB87C79C}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL=https://github.com/gakonst/nanocodex
AppSupportURL=https://github.com/gakonst/nanocodex/issues
DefaultDirName={userprofile}\.nanocodex\bin
UsePreviousAppDir=no
DefaultGroupName=Nanocodex
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
OutputDir=..\..\dist\windows-hand
OutputBaseFilename=nanocodex-hand-setup-x86_64
UninstallDisplayName=Nanocodex
VersionInfoVersion={#NumericVersion}
VersionInfoCompany={#AppPublisher}
VersionInfoDescription=Nanocodex Windows installer
VersionInfoProductName={#AppName}
VersionInfoProductVersion={#NumericVersion}
CloseApplications=yes
RestartApplications=no
ChangesEnvironment=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Files]
Source: "payload\nanocodex.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "payload\nanocodex2.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "payload\ffmpeg.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "payload\ffmpeg-*.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Set up or repair Nanocodex"; Filename: "{app}\nanocodex.exe"; Parameters: "setup --refresh"; WorkingDir: "{userprofile}"
Name: "{group}\Start Nanocodex Hand"; Filename: "{app}\nanocodex.exe"; Parameters: "hand start"; WorkingDir: "{userprofile}"
Name: "{group}\Stop Nanocodex Hand"; Filename: "{app}\nanocodex.exe"; Parameters: "hand stop"; WorkingDir: "{userprofile}"
Name: "{group}\Nanocodex Hand logs"; Filename: "{localappdata}\Nanocodex\Hand"
Name: "{group}\Uninstall Nanocodex"; Filename: "{uninstallexe}"

[Run]
Filename: "{app}\nanocodex.exe"; Parameters: "hand install"; Flags: runhidden waituntilterminated runasoriginaluser; Check: RepairExistingHand
Filename: "{app}\nanocodex.exe"; Parameters: "setup --refresh"; Description: "Sign in and connect this computer now"; Flags: postinstall waituntilterminated skipifsilent runasoriginaluser; Check: NeedsGuidedSetup
Filename: "{app}\nanocodex.exe"; Parameters: "update --auto enable"; Flags: runhidden waituntilterminated runasoriginaluser

[UninstallRun]
Filename: "{app}\nanocodex.exe"; Parameters: "hand stop"; Flags: runhidden waituntilterminated skipifdoesntexist; RunOnceId: "StopNanocodexHandTask"
Filename: "{app}\nanocodex.exe"; Parameters: "update --auto disable"; Flags: runhidden waituntilterminated skipifdoesntexist; RunOnceId: "RemoveNanocodexUpdateTask"
Filename: "{sys}\schtasks.exe"; Parameters: "/Delete /TN ""\Nanocodex Hand"" /F"; Flags: runhidden waituntilterminated; RunOnceId: "RemoveNanocodexHandTask"

[Messages]
WelcomeLabel2=This installs the Nanocodex CLI and its native Windows Hand.%n%nAfter installation, one guided setup signs in with a phone number, installs the official Computer Use components, starts the per-user Hand, and offers the browser extension. The connected account can control this user's apps and files. Desktop control requires a signed-in Windows session.

[Code]
var
  HadNativeHand: Boolean;

function RepairExistingHand: Boolean;
begin
  Result := HadNativeHand;
end;

function NeedsGuidedSetup: Boolean;
begin
  Result := not HadNativeHand;
end;

function NormalizedPathEntry(Value: String): String;
begin
  Result := Uppercase(RemoveBackslashUnlessRoot(Trim(Value)));
end;

procedure SetCommandPath(Add: Boolean);
var
  Existing: String;
  Remaining: String;
  Entry: String;
  Updated: String;
  Separator: Integer;
  InstallDir: String;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Existing) then
    Existing := '';
  Remaining := Existing;
  Updated := '';
  InstallDir := NormalizedPathEntry(ExpandConstant('{app}'));
  while Remaining <> '' do
  begin
    Separator := Pos(';', Remaining);
    if Separator = 0 then
    begin
      Entry := Remaining;
      Remaining := '';
    end
    else
    begin
      Entry := Copy(Remaining, 1, Separator - 1);
      Delete(Remaining, 1, Separator);
    end;
    if (Entry <> '') and (NormalizedPathEntry(Entry) <> InstallDir) then
    begin
      if Updated <> '' then Updated := Updated + ';';
      Updated := Updated + Entry;
    end;
  end;
  if Add then
  begin
    if Updated <> '' then Updated := Updated + ';';
    Updated := Updated + ExpandConstant('{app}');
  end;
  if Updated <> Existing then
    RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Updated);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then SetCommandPath(True);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then SetCommandPath(False);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ResultCode: Integer;
  ExistingCli: String;
begin
  Result := '';
  if Exec(ExpandConstant('{sys}\sc.exe'), 'query NanocodexHand', '', SW_HIDE,
    ewWaitUntilTerminated, ResultCode) and (ResultCode = 0) then
  begin
    Result := 'The retired machine-wide Nanocodex Hand service is still installed. ' +
      'Uninstall the previous Nanocodex Hand from Windows Settings once, then run this installer again.';
  end;
  if Result = '' then
  begin
    HadNativeHand := Exec(ExpandConstant('{sys}\schtasks.exe'),
      '/Query /TN "\Nanocodex Hand"', '', SW_HIDE, ewWaitUntilTerminated,
      ResultCode) and (ResultCode = 0);
    if HadNativeHand then
    begin
      ExistingCli := ExpandConstant('{app}\nanocodex.exe');
      if not FileExists(ExistingCli) then
        Result := 'A scheduled task named Nanocodex Hand already exists, but it was not installed by this application. Remove that task or repair its owning application first.'
      else if (not Exec(ExistingCli, 'hand stop', '', SW_HIDE,
        ewWaitUntilTerminated, ResultCode)) or (ResultCode <> 0) then
        Result := 'Could not stop the existing Nanocodex Hand safely. Run nanocodex hand stop and retry.';
    end;
  end;
end;
