; tri7 Wallet — Cryptographic Triangles (TRI) NSIS installer
; ONE file installs ONE program: tri7_wallet_gui.exe IS the wallet AND the
; node (welded in-process, src/node_embedded.rs). No follow_tip.exe, no
; tri7.exe, no txhistory.exe in the user's folder.
; VERSION: overridable from CI (makensis -DVERSION=<ver>) so the installer
; metadata AND the output filename carry the Cargo.toml version — the in-app
; auto-updater (src/updater.rs) resolves release assets as
; CryptographicTriangles-<ver>-win64-setup.exe.

!define APPNAME "Cryptographic Triangles"
!define COMPANY "Triangles"
!ifndef VERSION
!define VERSION "7.0.2"
!endif
!define EXEDIR "."

Name "${APPNAME} ${VERSION}"
OutFile "CryptographicTriangles-${VERSION}-win64-setup.exe"
InstallDir "$LOCALAPPDATA\Tri7Wallet"
InstallDirRegKey HKCU "Software\Tri7Wallet" "InstallDir"
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"
!include "FileFunc.nsh"
!define MUI_ICON "assets\triangles.ico"
!define MUI_ABORTWARNING

!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"

; ---------------------------------------------------------------------
; The wallet lives in ONE folder: wallet.dat, the node's chain store and
; the programs sit together, so the operator can copy that folder to move
; the wallet. The install folder therefore IS the wallet folder:
;   <wallet folder>\wallet.dat        the wallet (never touched by us)
;   <wallet folder>\bin\*.exe         the GUI wallet, the node, the CLI
;   <wallet folder>\node\state\...     the chain store the GUI's node uses
;   <wallet folder>\node\triangles.conf the node's config
; The GUI is launched with --wallet <folder>\wallet.dat so it always opens
; THIS wallet, and the node shortcut uses the same folder - the two can
; never end up fighting over different state directories.
; ---------------------------------------------------------------------

Function .onInit
  ; Pick the install folder (= the wallet folder). NOTE: no Abort here - in
  ; an .onInit callback Abort terminates the whole installer instead of
  ; ending the search, so the folder is selected with plain jumps.

  ; 1. a remembered install wins
  ReadRegStr $0 HKCU "Software\Tri7Wallet" "InstallDir"
  IfFileExists "$0\bin\tri7_wallet_gui.exe" 0 remembered_done
    StrCpy $INSTDIR "$0"
    Goto done
remembered_done:

  ; 2. the folder this installer was RUN FROM: drop the installer into the
  ;    wallet's folder and it installs itself right beside the wallet
  GetFullPathName $0 $EXEDIR
  IfFileExists "$0\wallet.dat" 0 exedir_done
    StrCpy $INSTDIR "$0"
    Goto done
exedir_done:

  ; 3. the default per-user folder (the MUI directory page below still lets
  ;    the operator browse to any existing wallet folder)
  StrCpy $INSTDIR "$LOCALAPPDATA\Tri7Wallet"
done:
FunctionEnd

Section "Install"
  SetOutPath "$INSTDIR\bin"
  ; ONE program: the wallet with the node welded in. Legacy installs that
  ; carried follow_tip.exe / tri7.exe / txhistory.exe keep those files on
  ; disk (the GUI ignores them — harmless); the uninstaller removes them.
  File "${EXEDIR}\tri7_wallet_gui.exe"

  ; the chain store + config live beside the wallet: the GUI's node
  ; supervisor uses <wallet folder>\node\state and
  ; <wallet folder>\node\triangles.conf (verified in src/gui/app.rs +
  ; src/gui/node_supervisor.rs)
  CreateDirectory "$INSTDIR\node\state"

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

  ; shortcuts
  CreateDirectory "$SMPROGRAMS\${APPNAME}"
  ; legacy shortcut cleanup: upgrades over pre-weld installs must not leave
  ; launchers for exes that no longer ship (a stray follow_tip.exe on disk
  ; from an old install stays harmless, but its shortcut is gone)
  Delete "$SMPROGRAMS\${APPNAME}\tri7d daemon.lnk"
  Delete "$SMPROGRAMS\${APPNAME}\${APPNAME} node (staking).lnk"
  ; the wallet shortcut opens THIS folder's wallet — it IS the node now
  CreateShortcut "$SMPROGRAMS\${APPNAME}\${APPNAME} Wallet.lnk" \
    "$INSTDIR\bin\tri7_wallet_gui.exe" '--wallet "$INSTDIR\wallet.dat"' \
    "$INSTDIR\bin\tri7_wallet_gui.exe" 0
  CreateShortcut "$SMPROGRAMS\${APPNAME}\Uninstall.lnk" \
    "$INSTDIR\uninstall.exe" "" "$INSTDIR" 0
  CreateShortcut "$DESKTOP\${APPNAME} Wallet.lnk" \
    "$INSTDIR\bin\tri7_wallet_gui.exe" '--wallet "$INSTDIR\wallet.dat"' \
    "$INSTDIR\bin\tri7_wallet_gui.exe" 0

  WriteUninstaller "$INSTDIR\uninstall.exe"

  ; EstimatedSize (KB, the unit Add/Remove Programs expects) measured from
  ; what this run ACTUALLY installed — not a stale constant. $0=size,
  ; $1=files, $2=dirs (FileFunc GetSize; recursion covers bin\ + node\).
  ; Runs after WriteUninstaller so the uninstaller itself is counted too.
  ; NOTE: on an upgrade over an existing folder this also counts
  ; pre-existing content (wallet.dat, chain store) — that IS ARP
  ; EstimatedSize semantics ("approximate size of the installed folder").
  ${GetSize} "$INSTDIR" "/S=0K" $0 $1 $2
  WriteRegDWORD HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet" \
    "EstimatedSize" "$0"
SectionEnd

Section "Uninstall"
  ; NEVER delete wallet.dat - it holds the operator's coins - and never
  ; delete triangles.conf (the operator's node settings: rpc credentials,
  ; transport choices). Only the programs and the re-downloadable chain
  ; store go.
  Delete "$INSTDIR\bin\tri7_wallet_gui.exe"
  ; legacy pre-weld installs may still carry these — clean them up
  Delete "$INSTDIR\bin\follow_tip.exe"
  Delete "$INSTDIR\bin\tri7.exe"
  Delete "$INSTDIR\bin\txhistory.exe"
  Delete "$INSTDIR\uninstall.exe"
  RMDir "$INSTDIR\bin"
  RMDir /r "$INSTDIR\node\state"
  RMDir /r "$INSTDIR\node\index"
  Delete "$DESKTOP\${APPNAME} Wallet.lnk"
  RMDir /r "$SMPROGRAMS\${APPNAME}"
  DeleteRegKey HKCU "Software\Microsoft\Windows\CurrentVersion\Uninstall\Tri7Wallet"
  DeleteRegKey HKCU "Software\Tri7Wallet"
SectionEnd
