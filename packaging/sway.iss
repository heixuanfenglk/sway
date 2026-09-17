; Sway Windows 安装包 — Inno Setup 6
; 由 packaging\build-installer.ps1 调用编译，也可在 Inno Compiler 中直接打开。
;
; 预定义（可由命令行 /D 覆盖）:
;   MyAppVersion   默认 0.1.0
;   MyAppSourceDir 默认 ..\target\release（相对本脚本目录）
;   MyAppOutputDir 默认 ..\dist

#ifndef MyAppVersion
  #define MyAppVersion "0.1.0"
#endif
#ifndef MyAppSourceDir
  #define MyAppSourceDir "..\target\release"
#endif
#ifndef MyAppOutputDir
  #define MyAppOutputDir "..\dist"
#endif

#define MyAppName "Sway"
#define MyAppNameFull "Sway · 全局代理"
#define MyAppPublisher "Sway"
#define MyAppURL "https://github.com"
#define MyAppExeName "sway.exe"
#define MyAppId "Sway.GlobalProxy"

[Setup]
AppId={{A8C3E5F1-9B2D-4E70-8F1A-6D4C2B0E9A75}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
AllowNoIcons=yes
OutputDir={#MyAppOutputDir}
OutputBaseFilename=Sway-{#MyAppVersion}-setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
UninstallDisplayName={#MyAppNameFull}
VersionInfoVersion={#MyAppVersion}.0
VersionInfoCompany={#MyAppPublisher}
VersionInfoDescription={#MyAppNameFull}
VersionInfoProductName={#MyAppName}
MinVersion=10.0
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "chinesesimp"; MessagesFile: "compiler:Languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#MyAppSourceDir}\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{group}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Comment: "{#MyAppNameFull}"
Name: "{group}\{cm:UninstallProgram,{#MyAppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; Comment: "{#MyAppNameFull}"

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchProgram,{#MyAppName}}"; Flags: nowait postinstall skipifsilent

[Code]
const
  InternetSettingsKey = 'Software\Microsoft\Windows\CurrentVersion\Internet Settings';
  EnvironmentKey = 'Environment';

procedure ClearSystemProxy;
begin
  { 卸载时清掉本程序可能留下的系统代理，避免无法上网 }
  RegWriteDWordValue(HKCU, InternetSettingsKey, 'ProxyEnable', 0);
  RegDeleteValue(HKCU, InternetSettingsKey, 'ProxyServer');

  RegDeleteValue(HKCU, EnvironmentKey, 'HTTP_PROXY');
  RegDeleteValue(HKCU, EnvironmentKey, 'HTTPS_PROXY');
  RegDeleteValue(HKCU, EnvironmentKey, 'ALL_PROXY');
  RegDeleteValue(HKCU, EnvironmentKey, 'http_proxy');
  RegDeleteValue(HKCU, EnvironmentKey, 'https_proxy');
  RegDeleteValue(HKCU, EnvironmentKey, 'all_proxy');
  RegDeleteValue(HKCU, EnvironmentKey, 'NO_PROXY');
  RegDeleteValue(HKCU, EnvironmentKey, 'no_proxy');
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    ClearSystemProxy;
end;
