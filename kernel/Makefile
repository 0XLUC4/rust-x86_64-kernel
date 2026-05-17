# =============================================================================
# Makefile — pipeline complet : assembleur + Rust -> ELF -> ISO bootable.
#
# Cibles principales :
#   make              : build tout
#   make run          : build + lance QEMU
#   make run-gdb      : build + lance QEMU, stoppé attendant GDB sur :1234
#   make clean        : nettoie
#
# Prérequis (Debian/Ubuntu) :
#   apt install nasm xorriso grub-pc-bin grub-common mtools qemu-system-x86
#   rustup component add rust-src llvm-tools-preview
# =============================================================================

arch ?= x86_64
kernel := build/kernel-$(arch).bin
iso := build/os-$(arch).iso
target := $(arch)-kernel
rust_kernel := target/$(target)/debug/librust_kernel.a

linker_script := linker.ld
grub_cfg := grub/grub.cfg
asm_sources := $(wildcard boot/*.asm)
asm_objects := $(patsubst boot/%.asm, build/boot/%.o, $(asm_sources))

.PHONY: all clean run run-gdb iso kernel rust cargo

all: $(kernel)

clean:
	@rm -rf build
	@cargo clean

# -----------------------------------------------------------------------------
# 1) Compile l'assembleur avec nasm (format elf64)
# -----------------------------------------------------------------------------
build/boot/%.o: boot/%.asm
	@mkdir -p $(shell dirname $@)
	nasm -felf64 $< -o $@

# -----------------------------------------------------------------------------
# 2) Compile le Rust en staticlib (cargo produit un ELF qu'on relink)
# -----------------------------------------------------------------------------
rust:
	cargo -Zjson-target-spec \
	      -Zbuild-std=core,compiler_builtins,alloc \
	      -Zbuild-std-features=compiler-builtins-mem \
	      build --lib --target x86_64-kernel.json

# -----------------------------------------------------------------------------
# 3) Link final : asm objects + Rust ELF -> kernel ELF multiboot2
# -----------------------------------------------------------------------------
$(kernel): $(asm_objects) $(linker_script) rust
	@mkdir -p $(shell dirname $@)
	ld -n --gc-sections -T $(linker_script) -o $(kernel) \
	   $(asm_objects) \
	   $(rust_kernel)
	@# Vérification : l'ELF DOIT contenir un header multiboot2 valide
	@grub-file --is-x86-multiboot2 $(kernel) \
	   && echo "✓ multiboot2 header OK" \
	   || (echo "✗ multiboot2 header manquant" && exit 1)

# -----------------------------------------------------------------------------
# 4) Construction de l'ISO bootable avec grub-mkrescue
# -----------------------------------------------------------------------------
$(iso): $(kernel) $(grub_cfg)
	@mkdir -p build/isofiles/boot/grub
	cp $(kernel) build/isofiles/boot/kernel.bin
	cp $(grub_cfg) build/isofiles/boot/grub
	grub-mkrescue -o $(iso) build/isofiles 2> /dev/null

iso: $(iso)

# -----------------------------------------------------------------------------
# 5) Création d'un disque FAT32 de test (si absent)
# -----------------------------------------------------------------------------
build/test.img:
	@mkdir -p build
	@echo "Création du disque FAT32 de test (32 MiB)..."
	dd if=/dev/zero of=build/test.img bs=1M count=32
	# Crée une table de partitions MBR + 1 partition FAT32
	printf 'o\nn\np\n1\n\n\nt\nc\nw\n' | fdisk build/test.img 2>/dev/null || true
	LOOP=$$(sudo losetup --find --show --offset 1048576 build/test.img) && \
	  sudo mkfs.vfat -F 32 -n RUSTDISK $$LOOP && \
	  sudo mkdir -p /tmp/rustmnt && \
	  sudo mount $$LOOP /tmp/rustmnt && \
	  echo "Hello from FAT32!" | sudo tee /tmp/rustmnt/hello.txt > /dev/null && \
	  echo "Rust Kernel test disk" | sudo tee /tmp/rustmnt/readme.txt > /dev/null && \
	  sudo mkdir -p /tmp/rustmnt/docs && \
	  echo "Documentation here" | sudo tee /tmp/rustmnt/docs/info.txt > /dev/null && \
	  sudo umount /tmp/rustmnt && \
	  sudo losetup -d $$LOOP
	@echo "✓ build/test.img prêt"

# Cible sans partition (disk image FAT32 brut, plus simple)
build/test-simple.img:
	@mkdir -p build
	@echo "Création du disque FAT32 simple (32 MiB)..."
	dd if=/dev/zero of=build/test-simple.img bs=1M count=32
	mkfs.vfat -F 32 -n RUSTDISK build/test-simple.img
	@echo "✓ build/test-simple.img prêt"

# QEMU flags communs
#   -machine pc,hpet=off : force le PIT comme source IRQ0 (sinon HPET capture
#                          IRQ0 sur Q35 / firmware moderne, et notre ISR PIC
#                          ne voit rien)
#   -machine pc          : chipset i440FX classique (plus simple que Q35 pour début
#                     kernel — Q35 route PIT via HPET via IOAPIC)
QEMU_COMMON := -serial stdio -m 128M -machine pc,hpet=off
QEMU_DISK   := $(shell test -f build/test.img && echo "-hda build/test.img" || echo "")
QEMU_NET    := -netdev user,id=net0 -device e1000,netdev=net0
QEMU_VIDEO  := -vga std

# -----------------------------------------------------------------------------
# 6) Run
# -----------------------------------------------------------------------------
run: $(iso)
	qemu-system-x86_64 -cdrom $(iso) $(QEMU_COMMON) $(QEMU_DISK) $(QEMU_NET) $(QEMU_VIDEO)

run-nodisk: $(iso)
	qemu-system-x86_64 -cdrom $(iso) $(QEMU_COMMON) $(QEMU_NET) $(QEMU_VIDEO)

# Mode GPU : -vga none + virtio-gpu-pci. Le driver virtio dans le kernel
# prend alors le relais de la présentation (TRANSFER+FLUSH au lieu de
# memcpy MMIO). Gain de perf attendu significatif pour des repaints
# pleine-fenêtre.
QEMU_GPU_VIDEO := -vga none -device virtio-gpu-pci
run-gpu: $(iso)
	qemu-system-x86_64 -cdrom $(iso) $(QEMU_COMMON) $(QEMU_DISK) $(QEMU_NET) $(QEMU_GPU_VIDEO)

run-gdb: $(iso)
	@echo "QEMU stoppé, en attente de GDB sur localhost:1234"
	@echo "Dans un autre terminal :"
	@echo "  gdb $(kernel) -ex 'target remote :1234'"
	qemu-system-x86_64 -cdrom $(iso) $(QEMU_COMMON) $(QEMU_DISK) $(QEMU_NET) $(QEMU_VIDEO) -s -S

# Run avec disque FAT32 de test (crée le disque si manquant)
run-fat: build/test.img $(iso)
	qemu-system-x86_64 -cdrom $(iso) $(QEMU_COMMON) -hda build/test.img $(QEMU_NET) $(QEMU_VIDEO)

