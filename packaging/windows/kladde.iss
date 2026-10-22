; Kladde installer for Windows (Inno Setup 6).
;   iscc /DVersion=0.1.2 /DExe=path\to\kladde.exe /DOutputDir=dist packaging\windows\kladde.iss
; Installs per user without admin rights, so Help > Check for updates can replace the program file.

#ifndef Version
  #error Pass /DVersion=<version>
#endif
#ifndef Exe
  #error Pass /DExe=<path to kladde.exe>
#endif
#ifndef OutputDir
  #define OutputDir "."
#endif

[Setup]
; Fixed forever: Windows recognises updates and the uninstall entry by this id.
AppId={{6F0D3C52-8B7E-4C41-9A3D-2E5B7C1F4A86}
AppName=Kladde
AppVersion={#Version}
AppVerName=Kladde {#Version}
AppPublisher=Jens
AppPublisherURL=https://github.com/SunYKnight/Kladde
AppSupportURL=https://github.com/SunYKnight/Kladde/issues
AppUpdatesURL=https://github.com/SunYKnight/Kladde/releases
PrivilegesRequired=lowest
DefaultDirName={autopf}\Kladde
DisableProgramGroupPage=yes
DisableDirPage=auto
LicenseFile=..\..\LICENSE
SetupIconFile=..\..\crates\kladde\assets\kladde.ico
UninstallDisplayIcon={app}\kladde.exe
UninstallDisplayName=Kladde
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
WizardStyle=modern
Compression=lzma2
SolidCompression=yes
CloseApplications=yes
OutputDir={#OutputDir}
OutputBaseFilename=kladde-x86_64-windows-setup

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#Exe}"; DestDir: "{app}"; DestName: "kladde.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Kladde"; Filename: "{app}\kladde.exe"
Name: "{autodesktop}\Kladde"; Filename: "{app}\kladde.exe"; Tasks: desktopicon

[Registry]
; Lists Kladde under "Open with" for any file.
Root: HKCU; Subkey: "Software\Classes\Applications\kladde.exe"; ValueType: string; ValueName: "FriendlyAppName"; \
  ValueData: "Kladde"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\Applications\kladde.exe\shell\open\command"; ValueType: string; \
  ValueData: """{app}\kladde.exe"" ""%1"""
Root: HKCU; Subkey: "Software\Classes\*\OpenWithList\kladde.exe"; Flags: uninsdeletekey

[Run]
Filename: "{app}\kladde.exe"; Description: "{cm:LaunchProgram,Kladde}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Left behind by a self-update until the next start.
Type: files; Name: "{app}\kladde.exe.old"
