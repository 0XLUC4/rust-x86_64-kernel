#!/usr/bin/env bash
# =============================================================================
# Runner appelé automatiquement par `cargo run` (via .cargo/config.toml).
# Note : `cargo run` seul ne suffit PAS — il construit le .bin Rust mais
# ne passe pas par nasm/ld/grub-mkrescue. Utilise `make run` à la place.
# Ce script sert surtout de fallback pour debug rapide de l'ELF Rust brut.
# =============================================================================
set -e

echo "⚠  Pour un boot propre, utilise 'make run'."
echo "   Ce runner ne lance que l'ELF Rust sans multiboot/GRUB."
echo ""

# Si tu veux vraiment essayer l'ELF Rust directement (ne bootera pas car
# pas de header multiboot ni de boot asm) :
#   qemu-system-x86_64 -kernel "$1"
exit 0
