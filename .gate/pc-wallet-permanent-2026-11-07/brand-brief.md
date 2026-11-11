# GUI brand asset replacement (operator-supplied originals)

Operator reported the wallet GUI showing a wrong logo/wordmark. The repo's
brand assets were reconstructions; the operator supplied the correct original
(C:/Users/hello/Dropbox/CoderBot/TRI logo w name new 3.png, 1199x252).

Change (binary assets only, no code):
- brand_header_new@2x.png = operator original resized 600x126 (was a blocky
  reconstruction at the same size); brand_header_new.png = 300x63 downscale.
- brand_mark_{16,20,32,38,64,128,256,1024}.png = the triangle mark cropped
  from the operator original (bbox of content pixels in the left 40%), padded
  square, LANCZOS-scaled. Transparent background.
- Originals preserved in .gate/pc-wallet-permanent-2026-11-07/brand-backup/.
- The old assets drew a pixelated imitation (verified by eye); the new assets
  are the operator's own artwork at the same dimensions/aspect (4.76:1), so no
  layout code changes.

Deploy path: CI (windows-build job) rebuilds tri7_wallet_gui.exe with the new
embedded assets; artifact tri7-windows is installed to the PC's
AppData Tri7Wallet bin and the GUI relaunched.
