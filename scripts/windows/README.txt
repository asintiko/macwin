MacWin for Windows
==================

Shares one keyboard, mouse and clipboard between this PC and a Mac on the same network.

Install
-------
Simplest: in PowerShell run
  powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/djjdjdkdjdvdvd/macwin/releases/latest/download/install-windows.ps1 | iex"

Or from this folder:
1. Extract this folder.
2. Double-click install.cmd (or run: powershell -ExecutionPolicy Bypass -File install.ps1).
   It installs MacWin for this user in %LOCALAPPDATA%\Programs\macwin, adds it to PATH and the
   Start menu, starts it in the background at login, allows it through Windows Firewall on
   private networks (Windows asks for administrator rights once) and opens the MacWin window.
   For a custom location, pass -Destination "path" to install.ps1; its copied uninstaller
   defaults to that folder.

Set up
------
1. On the Mac (the primary computer) choose "Primary" in the MacWin window and place this PC
   next to its screens.
2. Here choose "Secondary": MacWin shows a code such as 123-456.
3. Enter the code on the Mac. Both windows then show "Connected".
If the Mac cannot find this PC, choose "Enter the address manually" there and type the
address shown under the code here.

Closing the window keeps MacWin running in the notification area; click its icon to open the
window again, right-click it to pause or quit.

Notes
-----
- The network must be Private (Settings > Network & internet > Properties), or install with
  -AllNetworks to allow MacWin on Public networks too.
- The test build is not signed: if SmartScreen warns about a downloaded file, use the
  PowerShell command above, or choose More info > Run anyway.
- Emergency release: Ctrl+Alt+Shift+Esc.
- Log: %LOCALAPPDATA%\macwin\macwin.log

Uninstall: run uninstall.ps1 in the install folder. Add -Purge to delete all MacWin data,
including settings, identity, logs and received files.
Unrelated files in the installation folder are kept; a nonempty folder is not removed.
