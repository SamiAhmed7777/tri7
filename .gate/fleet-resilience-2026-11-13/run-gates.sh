#!/bin/bash
# fleet-resilience gate chain: lib tests -> GUI featureful check -> windows-gnu release
cd /root/tri7 || exit 9
LOG=/root/tri7/.gate/fleet-resilience-2026-11-13/gates.log
{
echo "=== cargo test --lib ==="
/root/.cargo/bin/cargo test --lib 2>&1 | tail -30
echo "EXIT_TEST=${PIPESTATUS[0]}"
echo "=== GUI featureful check ==="
CARGO_TARGET_DIR=/root/tri7/target-gui /root/.cargo/bin/cargo check --bin tri7_wallet_gui --features gui 2>&1 | tail -8
echo "EXIT_GUI=${PIPESTATUS[0]}"
echo "=== windows-gnu release all bins ==="
/root/.cargo/bin/cargo build --release --target x86_64-pc-windows-gnu --bins 2>&1 | tail -4
echo "EXIT_WIN=${PIPESTATUS[0]}"
echo "=== GATES DONE ==="
} >> "$LOG" 2>&1
