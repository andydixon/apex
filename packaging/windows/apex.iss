; Inno Setup 6 script for Apex. Compiled by packaging/windows/build.ps1 with
; /DAppVersion=<version> /DSourceDir=<repo root>.
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\.."
#endif

[Setup]
AppId={{7E3A1F4C-6B2D-4F0E-9C1A-3F5D2A7B9C10}
AppName=Apex
AppVersion={#AppVersion}
AppPublisher=Apex contributors
AppPublisherURL=https://github.com/andydixon/apex
DefaultDirName={autopf}\Apex
DefaultGroupName=Apex
UninstallDisplayIcon={app}\apex.exe
OutputBaseFilename=Apex-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesInstallIn64BitMode=x64compatible
ArchitecturesAllowed=x64compatible
LicenseFile={#SourceDir}\LICENSE
SetupIconFile={#SourceDir}\assets\icons\apex.ico
ChangesAssociations=yes
ChangesEnvironment=yes
PrivilegesRequiredOverridesAllowed=dialog

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "openwith"; Description: "Add ""Open with Apex"" to the Explorer context menu for files and folders"; GroupDescription: "Integration:"
Name: "assoc"; Description: "Open .txt, .md, .rs, .py, .js, .ts, .json, .toml, .yaml and .log files with Apex"; GroupDescription: "Integration:"; Flags: unchecked
Name: "path"; Description: "Add Apex to PATH so ""apex file.txt"" works in a terminal"; GroupDescription: "Integration:"
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

[Files]
Source: "{#SourceDir}\target\release\apex.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\Apex"; Filename: "{app}\apex.exe"
Name: "{group}\Uninstall Apex"; Filename: "{uninstallexe}"
Name: "{autodesktop}\Apex"; Filename: "{app}\apex.exe"; Tasks: desktopicon

[Registry]
; "Open with Apex" on every file and folder; the keys are removed on uninstall.
Root: HKA; Subkey: "Software\Classes\*\shell\Apex"; ValueType: string; ValueData: "Open with Apex"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\*\shell\Apex"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\apex.exe"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\*\shell\Apex\command"; ValueType: string; ValueData: """{app}\apex.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\shell\Apex"; ValueType: string; ValueData: "Open folder with Apex"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\shell\Apex"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\apex.exe"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\shell\Apex\command"; ValueType: string; ValueData: """{app}\apex.exe"" ""%1"""; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Apex"; ValueType: string; ValueData: "Open folder with Apex"; Flags: uninsdeletekey; Tasks: openwith
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Apex\command"; ValueType: string; ValueData: """{app}\apex.exe"" ""%V"""; Tasks: openwith
; ProgId for the opt-in associations (listed in "Open with" and usable as the default).
Root: HKA; Subkey: "Software\Classes\Apex.Document"; ValueType: string; ValueData: "Text document (Apex)"; Flags: uninsdeletekey; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Apex.Document\DefaultIcon"; ValueType: string; ValueData: """{app}\apex.exe"",0"; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Apex.Document\shell\open\command"; ValueType: string; ValueData: """{app}\apex.exe"" ""%1"""; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\Applications\apex.exe\shell\open\command"; ValueType: string; ValueData: """{app}\apex.exe"" ""%1"""; Flags: uninsdeletekey; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.txt\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.md\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.rs\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.py\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.js\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.ts\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.json\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.toml\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.yaml\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.yml\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
Root: HKA; Subkey: "Software\Classes\.log\OpenWithProgids"; ValueType: string; ValueName: "Apex.Document"; ValueData: ""; Flags: uninsdeletevalue; Tasks: assoc
; PATH entry (per-user or machine depending on the chosen privilege level).
Root: HKA; Subkey: "{code:PathRegKey}"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: path; Check: NeedsAddPath('{app}')

[Code]
function PathRegKey(Param: string): string;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function NeedsAddPath(Param: string): boolean;
var
  OrigPath: string;
begin
  if not RegQueryStringValue(HKA, PathRegKey(''), 'Path', OrigPath) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Param) + ';', ';' + Uppercase(OrigPath) + ';') = 0;
end;

[UninstallDelete]
Type: filesandordirs; Name: "{app}"
