#define AppName "BananaSplit"
#define AppVersion "2.5.0"
#define AppPublisher "LudaLunizz"
#define AppExe "dist\bananasplit.exe"

[Setup]
AppId={{2D1B240D-9DF2-42DA-AB10-095D01321250}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
DefaultDirName={localappdata}\Programs\BananaSplit
DefaultGroupName=BananaSplit
UninstallDisplayIcon={app}\bananasplit.exe
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.10240
OutputDir=dist
OutputBaseFilename=BananaSplit-Setup-v{#AppVersion}-x64
Compression=lzma2/ultra64
SolidCompression=yes
WizardStyle=modern
DisableProgramGroupPage=yes
SetupLogging=yes

[Files]
Source: "{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\BananaSplit"; Filename: "{app}\bananasplit.exe"
Name: "{autodesktop}\BananaSplit"; Filename: "{app}\bananasplit.exe"; Tasks: desktopicon

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Additional shortcuts:"; Flags: unchecked

[Run]
Filename: "{app}\bananasplit.exe"; Description: "Launch BananaSplit"; Flags: postinstall nowait skipifsilent
