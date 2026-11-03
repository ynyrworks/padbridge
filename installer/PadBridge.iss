; Inno Setup script for the Windows installer.
;   iscc /DAppVersion=0.4.0 installer\PadBridge.iss
; Builds dist\PadBridge-<version>-setup.exe from target\release.
;
; Installs per user by default (no administrator rights), adds Start menu
; and optional desktop shortcuts, and registers an uninstaller. It changes
; no system settings. Pairing keys and settings in %APPDATA%\PadBridge are
; left in place on uninstall; delete that folder to remove them.

#ifndef AppVersion
  #error Pass /DAppVersion=<version>
#endif

[Setup]
AppId={{4F58B5B7-96D7-4423-B674-9DE9AA3A0DDD}
AppName=PadBridge
AppVersion={#AppVersion}
AppVerName=PadBridge {#AppVersion}
AppPublisher=PadBridge contributors
AppPublisherURL=https://github.com/ynyrworks/padbridge
AppSupportURL=https://github.com/ynyrworks/padbridge/issues
VersionInfoVersion={#AppVersion}
VersionInfoProductName=PadBridge
DefaultDirName={autopf}\PadBridge
DefaultGroupName=PadBridge
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
LicenseFile=..\LICENSE
OutputDir=..\dist
OutputBaseFilename=PadBridge-{#AppVersion}-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
UninstallDisplayIcon={app}\PadBridge.exe
CloseApplications=yes

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\PadBridge.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\target\release\padbridge-cli.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"
Source: "..\THIRD_PARTY_NOTICES.md"; DestDir: "{app}"
Source: "..\licenses\*"; DestDir: "{app}\licenses"; Flags: recursesubdirs

[Icons]
Name: "{group}\PadBridge"; Filename: "{app}\PadBridge.exe"
Name: "{group}\{cm:UninstallProgram,PadBridge}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\PadBridge"; Filename: "{app}\PadBridge.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\PadBridge.exe"; Description: "{cm:LaunchProgram,PadBridge}"; Flags: nowait postinstall skipifsilent
