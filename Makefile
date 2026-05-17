# =============================================================================
# OS/Makefile — orchestration racine de l'OS.
#
# Architecture :
#   kernel/   : noyau Rust bare-metal x86_64
#   userland/ : programmes user ring 3 (ELFs compilés depuis la crate ulib)
#   tools/    : scripts de build (ex: gen disque FAT32)
#   docs/     : docs design + PR notes
#
# Cibles principales :
#   make            — build tout (userland puis kernel)
#   make run        — build + QEMU
#   make run-gdb    — idem + GDB server
#   make clean      — nettoie build/ partout
# =============================================================================

.PHONY: all kernel userland run run-gpu run-gdb run-nodisk clean

all: kernel

# Le kernel embarque userland/target/x86_64-user/release/{init,sh} via
# include_bytes!, donc userland doit être compilé d'abord.
kernel: userland
	$(MAKE) -C kernel

userland:
	@echo "[os] userland : build init + sh + displayd"
	cd userland && cargo -Zjson-target-spec build --release -p init -p sh -p displayd

run: kernel
	$(MAKE) -C kernel run

# Mode GPU accéléré : -vga none + virtio-gpu-pci.
# Le driver virtio-gpu prend la main sur la présentation (TRANSFER+FLUSH).
run-gpu: kernel
	$(MAKE) -C kernel run-gpu

run-gdb: kernel
	$(MAKE) -C kernel run-gdb

run-nodisk: kernel
	$(MAKE) -C kernel run-nodisk

clean:
	$(MAKE) -C kernel clean
	cd userland && cargo clean
