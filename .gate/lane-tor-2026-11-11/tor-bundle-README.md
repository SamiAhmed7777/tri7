# Bundled tor — the REAL Tor daemon shipped with the tri7 wallet

Sami, 2026-11-11 ("I want tor Fully integrated into the wallet so that it
should work on the laptop as well"): a wallet downloaded today must dial
.onion peers on a fresh Windows machine with no local tor. This payload is
the **unmodified official tor daemon** — no arti, no fork, no protocol code
of our own. CI (`.github/workflows/ci.yml`, windows job) downloads it,
verifies the pinned digest, and drops `tor.exe` here; the NSIS installer
places it under the app dir as `<app>\tor\tor.exe`, and the node supervisor
(`src/gui/node_supervisor.rs`) runs it as a hidden child with a generated
client-only torrc.

## Pinned payload (verified byte-for-byte against dist.torproject.org)

| field | value |
|---|---|
| version | **16.0.1** (latest torbrowser release shipping a Windows expert bundle; 16.0.2 is Android-only) |
| file | `tor-expert-bundle-windows-x86_64-16.0.1.tar.gz` |
| URL | `https://dist.torproject.org/torbrowser/16.0.1/tor-expert-bundle-windows-x86_64-16.0.1.tar.gz` |
| SHA256 | `9eed3c3ff1fe43df034ce360196c9ec95472560d77ff55614d0cd5120edea27b` |
| source of digest | official signed `sha256sums-signed-build.txt` for 16.0.1 |

To bump: change the version + digest in **both** the CI step (hardcoded
`$ver` / `$want`) and this file, verify the digest against the signed
`sha256sums-signed-build.txt` from the same release directory, and rebuild.

## What ships

`tor.exe` only. The 16.0.1 windows expert bundle tor binary is statically
linked (no DLLs); geoip data, pluggable transports and docs from the bundle
are deliberately not shipped — the wallet uses tor as a client-only SOCKS
provider, nothing else.

## Runtime contract (node supervisor)

- a **system tor already answering on 127.0.0.1:9050 is used instead** — the
  bundled daemon is never spawned on top of it (already-answer rule);
- otherwise `tor.exe -f <node state dir>\torrc` runs hidden (CREATE_NO_WINDOW),
  with a generated client-only torrc: explicit free `SocksPort 127.0.0.1:<p>`,
  `DataDirectory` under the node state dir, `ClientOnly 1`, `SafeLogging 1`,
  **no ControlPort** (control socket disabled by omission);
- startup wait is bounded (~30 s); on failure the wallet logs loudly and
  continues WITHOUT tor (clearnet bootstrap peers still work) — tor never
  blocks the wallet;
- the child is killed when the GUI exits.

## License / attribution

tor is copyright (c) The Tor Project, Inc. and distributed under the
3-clause BSD license:

> Redistribution and use in source and binary forms, with or without
> modification, are permitted provided that the following conditions are
> met:
> 1. Redistributions of source code must retain the above copyright notice,
>    this list of conditions and the following disclaimer.
> 2. Redistributions in binary form must reproduce the above copyright
>    notice, this list of conditions and the following disclaimer in the
>    documentation and/or other materials provided with the distribution.
> 3. Neither the names of the copyright holders nor the names of its
>    contributors may be used to endorse or promote products derived from
>    this software without specific prior written permission.
>
> THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS
> IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
> THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
> PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
> CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
> EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
> PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
> PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
> LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
> NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
> SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

Upstream: https://www.torproject.org — source:
https://dist.torproject.org and https://gitlab.torproject.org/tpo/core/tor.
The tri7 wallet does not modify the tor binary in any way; this file is the
attribution note that ships alongside it as `TOR-LICENSE.txt`.
