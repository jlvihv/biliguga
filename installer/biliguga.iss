#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{A9F5141E-DB4F-4BC2-8C72-2D6D6E6A7C31}
AppName=哔哩咕嘎
AppVersion={#AppVersion}
AppPublisher=jlvihv
DefaultDirName={localappdata}\Programs\biliguga
DefaultGroupName=哔哩咕嘎
OutputDir=..\dist
OutputBaseFilename=biliguga-windows-x86_64-setup
Compression=lzma2
SolidCompression=yes
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
UninstallDisplayName=哔哩咕嘎

[Files]
Source: "..\dist\biliguga\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
Name: "{autoprograms}\哔哩咕嘎"; Filename: "{app}\biliguga.exe"
Name: "{autodesktop}\哔哩咕嘎"; Filename: "{app}\biliguga.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "附加快捷方式："

[Run]
Filename: "{app}\biliguga.exe"; Description: "启动哔哩咕嘎"; Flags: nowait postinstall skipifsilent
