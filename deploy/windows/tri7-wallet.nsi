; tri7 Wallet — Cryptographic Triangles (TRI) NSIS installer
; v7.0.0 — installs the GUI wallet + tri7d daemon like a normal Windows program

!define APPNAME "Cryptographic Triangles"
!define COMPANY "Triangles"
!define VERSION "7.0.0"
!define EXEDIR "."

Name "${APPNAME} ${VERSION}"
OutFile "tri7-wallet-setup-7.0.0.exe"
InstallDir "$LOCALAPPDATA\Tri7Wallet"
InstallDirRegKey HKCU "Software\Tri7Wallet" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!define MUI_ICON "assets\triangles.ico"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

Section "Install"
  SetOutPath "$INSTDIR\bin"
  File "${EXEDIR}\tri7_wallet_gui.exe"
  File "${EXEDIR}\follow_tip.exe"

  SetOutPath "$INSTDIR\conf"
  FileOpen $0 "$INSTDIR\conf\triangles.conf" w
  FileWrite $0 "rpcbind=127.0.0.1$\r$\n"
  FileWrite $0 "rpcport=19114$\r$\n"
  FileWrite $0 "listen=1$\r$\n"
  FileWrite $0 "port=24113$\r$\n"
  FileWrite $0 "staking=0$\r$\n"
  FileWrite $0 "$\r$\n; Tor/I2P transports (optional)$\r$\n"
  FileWrite $0 ";torsocks=127.0.0.1:9050$\r$\n"
  FileWrite $0 ";i2psocks=127.0.0.1:4447$\r$\n"
  FileClose $0

  ; registry for reinstall path memory + uninstall entry
  WriteRegStr HKCU "Software\Tri7Wallet" "InstallDir" $INSTDIR
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet" \
    "DisplayName" "${APPNAME} ${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet" \
    "DisplayVersion" "${VERSION}"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet" \
    "UninstallString" "$INSTDIR\uninstall.exe"
  WriteRegStr HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet" \
    "Publisher" "${COMPANY}"
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet" \
    "EstimatedSize" 25

  ; shortcuts
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME} Wallet.lnk" \
    "$INSTDIR\bin\tri7_wallet_gui.exe" "" "$INSTDIR\bin\tri7_wallet_gui.exe" 0
  CreateShortcut "$SMPROGRAMS\${APPNAME}\tri7d daemon.lnk" \
    "$INSTDIR\bin\follow_tip.exe" \
    '--rpc-port 19114 --conf "$INSTDIR\conf\triangles.conf" --state-dir "$INSTDIR\state" --forever' \
    "$INSTDIR\bin\follow_tip.exe" 0
  CreateShortcut "$DESKTOP\${APPNAME} Wallet.lnk" \
    "$INSTDIR\bin\tri7_wallet_gui.exe" "" "$INSTDIR\bin\tri7_wallet_gui.exe" 0

  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Section "Uninstall"
  Delete "$INSTDIR\bin\tri7_wallet_gui.exe"
  Delete "$INSTDIR\bin\follow_tip.exe"
  Delete "$INSTDIR\conf\triangles.conf"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR\bin"
  RMDir "$INSTDIR\conf"
  RMDir /r "$INSTDIR\state"
  RMDir "$INSTDIR"
  Delete "$DESKTOP\${APPNAME} Wallet.lnk"
  RMDir /r "$SMPROGRAMS\${APPNAME}"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet"
  DeleteRegKey HKCU "Software\Tri7Wallet"
SectionEnd
