# AirPlay proxy build system

RELEASE_DIR = target/xtensa-esp32-espidf/release

# A Wi-Fi network and an AirPlay receiver may be built into the firmware, to hold until others are
# chosen on the page of the proxy. local.mk, which git ignores, sets them: see local.mk.example.
-include local.mk
export PROXY_WIFI_SSID PROXY_WIFI_PASSWORD PROXY_RECEIVER

.PHONY: all build flash monitor clean

all: build

build:
	@test -n "$$LIBCLANG_PATH" || { echo "Run 'source ~/export-esp.sh' first."; exit 1; }
	@env PATH="$$(dirname "$$(mise which python 2>/dev/null)"):$$PATH" cargo build --release

flash: build
	espflash flash --monitor --bootloader $(RELEASE_DIR)/bootloader.bin --partition-table partitions.csv --target-app-partition firmware $(RELEASE_DIR)/proxy

monitor:
	espflash monitor

clean:
	cargo clean
