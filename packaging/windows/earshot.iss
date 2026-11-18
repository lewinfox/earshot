; The Windows installer (Inno Setup 6), made by packaging/windows/package.sh from a
; release folder (packaging/bundle.sh), which passes Bundle, Version, NumericVersion
; and OutputDir/OutputName with /D.
;
; Installs for the current user by default, with no administrator prompt
; (%LOCALAPPDATA%\Programs\earshot); the first page offers all users instead. A new
; version installs over the old one. Uninstalling removes the program, and asks
; whether to remove what earshot keeps in the user's folders too (core/folders.rs).

#ifndef Version
  #error Version not defined (package.sh passes it)
#endif

[Setup]
; The same for every version, forever: it's how Windows knows a new one replaces it.
AppId={{4AAC3BB1-1D25-4990-89DA-80C694F8AFA7}
AppName=earshot
AppVersion={#Version}
AppVerName=earshot {#Version}
AppPublisher=Lewin Appleton-Fox
AppPublisherURL=https://github.com/lewinfox/earshot
AppSupportURL=https://github.com/lewinfox/earshot/issues
VersionInfoVersion={#NumericVersion}
DefaultDirName={autopf}\earshot
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
SetupIconFile=..\..\gui\assets\icons\earshot.ico
UninstallDisplayIcon={app}\earshot.exe
UninstallDisplayName=earshot
WizardStyle=modern
; Torch and friends: a solid archive compresses them much better.
Compression=lzma2
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
; Closes a running earshot before replacing it.
CloseApplications=yes

[Tasks]
Name: desktopicon; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

; What an older version installed, gone first, so a package or file it had and this
; one doesn't can't linger (Python would still import it).
[InstallDelete]
Type: filesandordirs; Name: "{app}\python"
Type: filesandordirs; Name: "{app}\worker"
Type: filesandordirs; Name: "{app}\models"
Type: filesandordirs; Name: "{app}\bin"
Type: filesandordirs; Name: "{app}\licenses"

[Files]
Source: "{#Bundle}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\earshot"; Filename: "{app}\earshot.exe"
Name: "{autodesktop}\earshot"; Filename: "{app}\earshot.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\earshot.exe"; Description: "{cm:LaunchProgram,earshot}"; Flags: nowait postinstall skipifsilent

[Code]
// Where core/folders.rs keeps things on Windows (directories' ProjectDirs for
// io.github / lewinfox / earshot): data and config roaming, cache and logs local.
procedure RemoveUserFolders;
begin
  DelTree(ExpandConstant('{userappdata}\lewinfox\earshot'), True, True, True);
  DelTree(ExpandConstant('{localappdata}\lewinfox\earshot'), True, True, True);
  // The lewinfox folders too, if nothing else is in them.
  RemoveDir(ExpandConstant('{userappdata}\lewinfox'));
  RemoveDir(ExpandConstant('{localappdata}\lewinfox'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  // Asked only when someone's there to answer: a silent uninstall keeps them.
  if (CurUninstallStep = usPostUninstall) and not UninstallSilent then
    if MsgBox('Also delete your earshot library, settings and analysis?' + #13#10 + #13#10 +
              'Your music files are not touched either way. Keep them if you might ' +
              'install earshot again.', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
      RemoveUserFolders;
end;
