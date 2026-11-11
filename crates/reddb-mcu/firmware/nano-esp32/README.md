# Nano ESP32 firmware receipt

This standalone ESP-IDF project links `reddb-io-mcu` for the Nano's dual-core
ESP32-S3. It mounts a wear-levelled FAT partition and preallocates one
contiguous `data.rdb` before the RedDB pager starts. Preallocation is board
policy; the bytes inside the file remain the canonical RedDB 1.33.3 `.rdb`
format.

The firmware creates or recovers the database, ingests 43,200 one-minute
measurements through the shared SQL executor, and streams the full history
through SQL. It prints `REDB_MCU_OK rows=43200 capacity_bytes=12378112` only
after checking every timestamp and temperature. No gateway is used.

## Build

Install the esp-rs `esp` toolchain, `ldproxy` and `espflash`, then build from
this directory:

```text
cargo +esp build --release
```

On Windows, a short external target and ESP-IDF tools directory avoid path
length failures. For example in PowerShell:

```powershell
$env:ESP_IDF_TOOLS_INSTALL_DIR = "custom:C:/.embuild/espressif"
cargo +esp build --release --target-dir C:\rdbt

$elf = "C:\rdbt\xtensa-esp32s3-espidf\release\reddb-mcu-nano-esp32"
espflash save-image --chip esp32s3 --partition-table partitions.csv `
  --flash-size 16mb --merge $elf build\reddb-mcu-nano-esp32.bin
Copy-Item $elf build\reddb-mcu-nano-esp32.elf
```

The app occupies 3,806,592 of the 4,194,304-byte app partition (90.76%). The
remaining flash partition yields a 12,378,112-byte contiguous database file.
The main task has a 256 KiB stack; the board exposes roughly 390 KiB of other
internal RAM plus 8 MiB octal PSRAM to the allocator.

## Emulate

The qualified emulator is Espressif `esp-emu` 0.51.0 or newer. On Linux:

```text
curl -fsSL https://raw.githubusercontent.com/espressif/esp-emulator/main/install.sh | sh
esp-emu \
  --chip esp32s3 \
  --firmware build/reddb-mcu-nano-esp32.bin \
  --elf build/reddb-mcu-nano-esp32.elf \
  --psram-size 8M \
  --batch-size 500000 \
  --exit-on REDB_MCU_OK \
  --save-state \
  --timeout 3600s
```

Run the same command again without `--save-state` to validate recovery. The
second boot must report `state=Existing`, must not print ingestion progress,
and must finish with the same `REDB_MCU_OK` receipt.

The older Espressif QEMU 9.2.2 model was not qualified for this sustained
FAT/WL workload: its SMP flash-cache model and write-back MTD backend aborted
mid-write. `esp-emu` completed both the dual-core first boot and recovery.

`diagram.json` and `wokwi.toml` provide an optional Wokwi ESP32-S3 surface with
16 MiB flash and 8 MiB octal PSRAM. Pin layout is intentionally not claimed;
the receipt covers CPU, memory, SQL and flash behavior. A physical Nano ESP32
run remains a separate qualification step.
