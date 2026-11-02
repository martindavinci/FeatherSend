; FeatherSend installer (Inno Setup 6). Per-user: no administrator rights.
; Built by `cargo xtask dist`, which passes Version, Arch and SourceExe.

#ifndef Version
  #define Version "0.0.0"
#endif
#ifndef Arch
  #define Arch "x64"
#endif
#ifndef SourceExe
  #error SourceExe must be defined (/DSourceExe=path\to\FeatherSend.exe)
#endif

[Setup]
AppId={{5B6E3C8A-1F2D-4E7B-9A3C-FEA7E85E0D01}
AppName=FeatherSend
AppVersion={#Version}
AppVerName=FeatherSend {#Version}
AppPublisher=Martin DaVinci
AppPublisherURL=https://github.com/martindavinci/FeatherSend
AppSupportURL=https://github.com/martindavinci/FeatherSend/issues
DefaultDirName={localappdata}\Programs\FeatherSend
DisableProgramGroupPage=yes
DisableDirPage=auto
PrivilegesRequired=lowest
MinVersion=6.1sp1
OutputBaseFilename=FeatherSend-{#Version}-{#Arch}-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupIconFile=..\app\res\FeatherSend.ico
UninstallDisplayIcon={app}\FeatherSend.exe
UninstallDisplayName=FeatherSend
LicenseFile=..\LICENSE
CloseApplications=yes
#if Arch == "x64"
; "x64" (not "x64compatible") so every Inno Setup 6.x accepts it.
ArchitecturesAllowed=x64
ArchitecturesInstallIn64BitMode=x64
#endif

[Languages]
Name: "en"; MessagesFile: "compiler:Default.isl"
Name: "es"; MessagesFile: "compiler:Languages\Spanish.isl"
Name: "fr"; MessagesFile: "compiler:Languages\French.isl"
Name: "de"; MessagesFile: "compiler:Languages\German.isl"
Name: "it"; MessagesFile: "compiler:Languages\Italian.isl"
Name: "ptbr"; MessagesFile: "compiler:Languages\BrazilianPortuguese.isl"
Name: "ja"; MessagesFile: "compiler:Languages\Japanese.isl"
; Not shipped with every Inno Setup 6.x, so kept here (see lang\README.txt).
Name: "ko"; MessagesFile: "lang\Korean.isl"
Name: "zhcn"; MessagesFile: "lang\ChineseSimplified.isl"

[CustomMessages]
en.AutoStart=Start FeatherSend with Windows
es.AutoStart=Iniciar FeatherSend con Windows
fr.AutoStart=Démarrer FeatherSend avec Windows
de.AutoStart=FeatherSend mit Windows starten
it.AutoStart=Avvia FeatherSend con Windows
ptbr.AutoStart=Iniciar o FeatherSend com o Windows
ja.AutoStart=Windows と同時に FeatherSend を起動
ko.AutoStart=Windows 시작 시 FeatherSend 실행
zhcn.AutoStart=随 Windows 启动 FeatherSend

[Tasks]
Name: "autostart"; Description: "{cm:AutoStart}"; Flags: unchecked
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "FeatherSend.exe"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion
Source: "..\NOTICE"; DestDir: "{app}"; DestName: "NOTICE.txt"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\FeatherSend"; Filename: "{app}\FeatherSend.exe"
Name: "{userdesktop}\FeatherSend"; Filename: "{app}\FeatherSend.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "FeatherSend"; ValueData: """{app}\FeatherSend.exe"" --minimized"; Tasks: autostart

[Run]
Filename: "{app}\FeatherSend.exe"; Description: "{cm:LaunchProgram,FeatherSend}"; Flags: nowait postinstall skipifsilent

[UninstallRun]
Filename: "{sys}\taskkill.exe"; Parameters: "/f /im FeatherSend.exe"; Flags: runhidden; RunOnceId: "StopFeatherSend"
Filename: "{sys}\reg.exe"; Parameters: "delete HKCU\Software\Microsoft\Windows\CurrentVersion\Run /v FeatherSend /f"; Flags: runhidden; RunOnceId: "RemoveAutostart"
