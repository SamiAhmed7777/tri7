# Windows installer: ONE file, installed INTO the wallet's folder

Operator: "The daemon and the GUI wallet should be combined together into a
single file and it should be in the folder where the wallet is ... I'm going
to install it on my laptop once it's properly ready to go", and "Is the wallet
properly installed on the start menu".

The installer (deploy/windows/tri7-wallet.nsi) had three defects:

1. IT DID NOT INSTALL INTO THE WALLET FOLDER. InstallDir defaulted to
   %LOCALAPPDATA%\Tri7Wallet regardless of where the wallet actually lives,
   so the programs and the wallet ended up in different places.
2. THE NODE SHORTCUT POINTED AT A STATE DIR THE WALLET NEVER USES
   ($INSTDIR\state, while the GUI's supervisor uses <wallet>\..\node\state).
   Launching it from the Start Menu started a SECOND node that fought the
   supervised one for the RPC port - the same conflict found and fixed by
   hand on the operator's PC and laptop.
3. THE INSTALLER SHIPPED ONLY TWO BINARIES - the CLI (tri7.exe) was not built
   into the Windows artifact at all.

Fixes:
- .onInit adopts an EXISTING wallet folder: a remembered install wins,
  otherwise E:\Coins\TRI, C:\Coins\TRI-LAPTOP, %LOCALAPPDATA%\Tri7Wallet
  (the MUI directory page still lets the operator pick another folder).
  The install folder IS the wallet folder:
    <folder>\wallet.dat      the wallet (never written by the installer)
    <folder>\bin\*.exe       GUI wallet + node + CLI
    <folder>\node\state\    the chain store the GUI's node uses
    <folder>\conf\           the node's config
- the wallet shortcut now passes --wallet "<folder>\wallet.dat", so it
  always opens THIS wallet instead of relying on a remembered path.
- the node shortcut uses the SAME folder and the SAME state dir the wallet's
  supervisor uses (--wallet, --state-dir <folder>\node\state, --conf
  <folder>\conf\triangles.conf, --rpc-port 19114, --rpc-bind 127.0.0.1).
- the uninstaller NEVER deletes wallet.dat (it holds the operator's coins);
  only the programs, the chain store and the shortcuts go.
- an Uninstall shortcut is created next to the other Start Menu entries.
- the installer payload now includes tri7.exe (the CLI), and the CI windows
  job builds it, stages it for NSIS and publishes it in the artifact.
