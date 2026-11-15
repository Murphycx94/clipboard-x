; ClipboardX's installer for Windows: for the current user (no
; administrator), a Start menu entry, optionally started with Windows.
; scripts\package-windows.ps1 compiles it:
;
;   ISCC /DAppVersion=<version> /DSourceExe=<ClipboardX.exe> /DOutputDir=<directory> /DOutputName=<file name> scripts\windows-installer.iss

#ifndef AppVersion
  #error AppVersion is not defined
#endif
#ifndef SourceExe
  #error SourceExe is not defined
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif
#ifndef OutputName
  #define OutputName "ClipboardX-" + AppVersion + "-windows-x86_64-setup"
#endif

[Setup]
; Identifies the app to later installers and the uninstaller: never change it.
AppId={{6F3C1B52-8E0A-4B7D-9C4E-2A9D5E7F1C36}
AppName=ClipboardX
AppVersion={#AppVersion}
AppPublisher=ClipboardX
AppPublisherURL=https://github.com/Murphycx94/clipboard-x
AppSupportURL=https://github.com/Murphycx94/clipboard-x/issues
DefaultDirName={localappdata}\Programs\ClipboardX
DisableDirPage=yes
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\ClipboardX.exe
UninstallDisplayName=ClipboardX
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
; An update closes the running app (it quits when asked to end its session).
CloseApplications=yes
RestartApplications=no

[Tasks]
Name: "autostart"; Description: "Start ClipboardX when I sign in to Windows"; GroupDescription: "Startup:"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "ClipboardX.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\ClipboardX"; Filename: "{app}\ClipboardX.exe"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "ClipboardX"; ValueData: """{app}\ClipboardX.exe"""; Flags: uninsdeletevalue; Tasks: autostart
; Unticked on a later install: the entry an earlier one wrote goes.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "ClipboardX"; Flags: deletevalue; Tasks: not autostart

[Run]
Filename: "{app}\ClipboardX.exe"; Description: "Start ClipboardX"; Flags: nowait postinstall skipifsilent

[UninstallRun]
; The app runs in the tray: it has to go before its file can.
Filename: "{sys}\taskkill.exe"; Parameters: "/IM ClipboardX.exe /F"; Flags: runhidden; RunOnceId: "StopClipboardX"
