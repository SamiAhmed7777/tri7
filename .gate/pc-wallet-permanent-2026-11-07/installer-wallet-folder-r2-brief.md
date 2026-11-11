# Windows installer: ONE installer file, installed INTO the wallet's folder

Operator: "The daemon and the GUI wallet should be combined together into a
single file and it should be in the folder where the wallet is ... I'm going
to install it on my laptop once it's properly ready to go", and "Is the wallet
properly installed on the start menu".

ONE installer file (.exe) carries the GUI wallet, the node and the CLI; the
installed payload is three programs in one folder.

The installer (deploy/windows/tri7-wallet.nsi) had three defects:

1. IT DID NOT INSTALL INTO THE WALLET FOLDER. InstallDir defaulted to
   %LOCALAPPDATA%\Tri7Wallet regardless of where the wallet actually lives,
   so the programs and the wallet ended up in different places.
2. THE NODE SHORTCUT POINTED AT A STATE DIR THE WALLET NEVER USES
   ($INSTDIR\state, while the GUI's supervisor uses <wallet folder>\node\
   state). Launching it from the Start Menu started a SECOND node that
   fought the supervised one for the RPC port - the same conflict found and
   fixed by hand on the operator's PC and laptop.
3. THE INSTALLER SHIPPED ONLY TWO BINARIES - the CLI (tri7.exe) was not built
   into the Windows artifact at all.

Fixes:
- .onInit selects the install folder (= the wallet folder) with plain jumps:
    1. a remembered install (HKCU\Software\Tri7Wallet\InstallDir) wins;
    2. otherwise the folder the installer was RUN FROM (drop the installer
       into the wallet's folder and it installs itself beside the wallet);
    3. otherwise %LOCALAPPDATA%\Tri7Wallet.
  The MUI directory page still lets the operator browse to any folder, so an
  existing wallet elsewhere can be chosen there. NO machine-specific path is
  hardcoded.
  (The first draft used Abort inside .onInit to end the search - in NSIS an
  Abort in that callback terminates the whole installer, which would have
  blocked the install entirely; it is now plain control flow.)
- the layout matches the GUI's node supervisor exactly (verified in
  src/gui/app.rs: node_dir = wallet.parent()/"node", and
  src/gui/node_supervisor.rs: state_dir = node_dir/"state", conf =
  node_dir/"triangles.conf"):
    <folder>\wallet.dat        the wallet (never written by the installer)
    <folder>\bin\*.exe         GUI wallet + node + CLI
    <folder>\node\state\...    the chain store the GUI's node uses
    <folder>\node\triangles.conf  the node's config
- the wallet shortcut passes --wallet "<folder>\wallet.dat", so it always
  opens THIS wallet instead of relying on a remembered path.
- the node shortcut uses the SAME folder, state dir and conf the wallet's
  supervisor uses (--wallet, --state-dir <folder>\node\state, --conf
  <folder>\node\triangles.conf, --rpc-port 19114, --rpc-bind 127.0.0.1).
- the uninstaller NEVER deletes wallet.dat (it holds the operator's coins);
  only the programs, the chain store and the shortcuts go.
- an Uninstall shortcut is created next to the other Start Menu entries.
- the installer payload now includes tri7.exe (the CLI), and the CI windows
  job builds it, stages it for NSIS and publishes it in the artifact.

Verified on the operator's machines by hand: the Start Menu entries were
repointed to the live binaries and folders (the wallet entry with its
--wallet argument, the node entry to the running launcher, and the stale
"tri7d daemon" entry - which pointed at a non-live conf/state pair - was
removed on both the PC and the laptop).
