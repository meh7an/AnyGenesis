; The Windows installer, compiled by `cargo xtask release` with Inno Setup 7, which passes
; AppVersion, SourceDir (the staged release folder) and OutputDir. A per-user install by
; default (no administrator prompt), into %LOCALAPPDATA%\Programs\AnyGenesis.

#ifndef AppVersion
  #error Compile this through cargo xtask release
#endif

[Setup]
AppId={{2F528813-33E4-4E8C-ABF7-16B145EC5593}
AppName=AnyGenesis
AppVersion={#AppVersion}
AppVerName=AnyGenesis {#AppVersion}
AppPublisher=Mehran
AppPublisherURL=https://github.com/meh7an/AnyGenesis
DefaultDirName={autopf}\AnyGenesis
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename=AnyGenesis-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\crates\gui\assets\icon.ico
UninstallDisplayIcon={app}\anygenesis-gui.exe
UninstallDisplayName=AnyGenesis
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[InstallDelete]
; An update replaces the toolchain whole, so no file of an older Zig is left behind.
Type: filesandordirs; Name: "{app}\toolchain"

[Icons]
Name: "{autoprograms}\AnyGenesis"; Filename: "{app}\anygenesis-gui.exe"
Name: "{autodesktop}\AnyGenesis"; Filename: "{app}\anygenesis-gui.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\anygenesis-gui.exe"; Description: "{cm:LaunchProgram,AnyGenesis}"; Flags: nowait postinstall skipifsilent
