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

  ; no starter config shipped: tri7d/GUI generate their own defaults on first
  ; run and the operator decides staking/transport settings in-app

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
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR\bin"
  RMDir /r "$INSTDIR\state"
  RMDir /r "$INSTDIR\conf"
  RMDir "$INSTDIR"
  Delete "$DESKTOP\${APPNAME} Wallet.lnk"
  RMDir /r "$SMPROGRAMS\${APPNAME}"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet"
  DeleteRegKey HKCU "Software\Tri7Wallet"
SectionEnd
